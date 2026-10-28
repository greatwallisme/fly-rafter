// Rust guideline compliant 2026-10-28
//! Advisory per-agent repeat-call detector (dsh
//! `guard/repeat-tool-reminder`, verbatim semantics).
//!
//! The guard enriches post-execute decisions with logged model context
//! without vetoing or rewriting calls: it counts consecutive identical calls
//! per agent (denied calls included — a model hammering a denied call is
//! exactly the loop worth breaking), delegates so later listeners still
//! decide, then folds a gentle-then-detailed reminder onto whatever came
//! back. A user interjection resets the chain; the reset hook (the fr
//! `agent/user-admitted` notification, decision Q102′A) is wired by the
//! plugin shell alongside the service face.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fr_tools::json_schema::js_number_to_string;
use fr_tools::registry::PostExecuteListener;
use fr_tools::types::{PostToolDecision, ToolExecution, ToolExecutionResult};
use fr_wire::Message;
use serde_json::{Value, json};

pub mod plugin_shell;

/// The gentle first-threshold reminder, keyed to `thresholds[0]`.
pub const GENTLE_REMINDER: &str = "You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call.";

/// One agent's consecutive-repeat chain.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chain {
    key: String,
    count: u64,
}

/// The validated guard configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardConfig {
    /// Consecutive-repeat counts that trigger a reminder, sorted ascending.
    pub thresholds: Vec<u64>,
    /// Tool-name patterns to track; empty tracks every tool.
    pub include: Vec<String>,
    /// Tool-name patterns transparent to the chain.
    pub exclude: Vec<String>,
    /// Maximum characters of canonical arguments in the detailed reminder.
    pub arguments_preview_chars: u64,
}

/// The repeat-call guard state.
pub struct RepeatGuard {
    config: GuardConfig,
    include: Vec<regex::Regex>,
    exclude: Vec<regex::Regex>,
    chains: Mutex<HashMap<String, Chain>>,
    reminder_ids: AtomicU64,
}

impl std::fmt::Debug for RepeatGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The compiled patterns and chain table stay summarized.
        let chain_count = recover(&self.chains).len();
        f.debug_struct("RepeatGuard")
            .field("config", &self.config)
            .field("include", &self.config.include)
            .field("exclude", &self.config.exclude)
            .field("chain_count", &chain_count)
            .finish_non_exhaustive()
    }
}

impl RepeatGuard {
    /// Validates the configuration fail-loud (dsh `validateThresholds` and
    /// the preview-cap check) and compiles the wildcard patterns.
    ///
    /// # Errors
    ///
    /// Returns the dsh load-time failure texts verbatim.
    pub fn try_new(config: &Value) -> Result<Arc<Self>, String> {
        let thresholds_raw = config
            .get("thresholds")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![json!(3), json!(5), json!(8)]);
        let thresholds = validate_thresholds(&thresholds_raw)?;
        let arguments_preview_chars = config
            .get("argumentsPreviewChars")
            .and_then(Value::as_f64)
            .unwrap_or(500.0);
        if arguments_preview_chars.fract() != 0.0 || arguments_preview_chars < 1.0 {
            #[expect(
                clippy::option_if_let_else,
                reason = "the map_or_else closure form fails inference on the fn-item argument"
            )]
            let rendered = match serde_json::Number::from_f64(arguments_preview_chars) {
                Some(number) => js_number_to_string(&number),
                None => arguments_preview_chars.to_string(),
            };
            return Err(format!(
                "repeat-tool-reminder: invalid argumentsPreviewChars {rendered} — must be an integer >= 1"
            ));
        }
        let read_patterns = |key: &str| -> Vec<String> {
            config
                .get(key)
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let include = read_patterns("include");
        let exclude = read_patterns("exclude");
        let compiled_include = include
            .iter()
            .map(|p| wildcard_to_regex(p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("repeat-tool-reminder: invalid include pattern: {error}"))?;
        let compiled_exclude = exclude
            .iter()
            .map(|p| wildcard_to_regex(p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("repeat-tool-reminder: invalid exclude pattern: {error}"))?;
        Ok(Arc::new(Self {
            config: GuardConfig {
                thresholds,
                include,
                exclude,
                arguments_preview_chars: integer_or_cap(arguments_preview_chars),
            },
            include: compiled_include,
            exclude: compiled_exclude,
            chains: Mutex::new(HashMap::new()),
            reminder_ids: AtomicU64::new(0),
        }))
    }

    /// The validated configuration.
    #[must_use]
    pub const fn config(&self) -> &GuardConfig {
        &self.config
    }

    /// Resets one agent's chain (the user-interjection hook; the shell wires
    /// it to the `agent/user-admitted` notification).
    pub fn reset_chain(&self, agent: &str) {
        recover(&self.chains).remove(agent);
    }

    /// Builds the observe-and-enrich post-execute listener: count first
    /// (state advances regardless of the downstream outcome), delegate, then
    /// fold the reminder onto the decision (both variants carry the context).
    #[must_use]
    pub fn listener(self: &Arc<Self>) -> PostExecuteListener {
        let guard = Arc::clone(self);
        Arc::new(
            move |exec: &ToolExecution,
                  _result: &ToolExecutionResult,
                  next: &mut dyn FnMut() -> PostToolDecision| {
                let reminder = guard.observe(exec);
                let downstream = next();
                fold_reminder(downstream, reminder)
            },
        )
    }

    /// Advances the agent's chain and returns the reminder to deliver when
    /// the run length hits a configured threshold (public for the shell's
    /// two-leg endpoint: the enter leg counts before delegating).
    pub fn observe(&self, exec: &ToolExecution) -> Option<Message> {
        // A direct registry caller has no model to remind and no id to key
        // on; only agent-loop calls participate.
        let agent = exec.agent.as_deref()?;
        if !self.tracked(&exec.name) {
            return None;
        }
        let canonical = canonicalize(&exec.arguments);
        let key = serde_json::to_string(&Value::Array(vec![
            Value::String(exec.name.clone()),
            Value::String(canonical.clone()),
        ]))
        .unwrap_or_default();
        let count = {
            let mut chains = recover(&self.chains);
            let count = match chains.get(agent) {
                Some(chain) if chain.key == key => chain.count + 1,
                _ => 1,
            };
            chains.insert(agent.to_owned(), Chain { key, count });
            count
        };
        if !self.config.thresholds.contains(&count) {
            return None;
        }
        let text = if count == self.config.thresholds[0] {
            GENTLE_REMINDER.to_owned()
        } else {
            detailed_reminder(
                &exec.name,
                count,
                &preview_arguments(&canonical, self.config.arguments_preview_chars),
            )
        };
        Some(self.reminder_message(&text, &exec.name, count))
    }

    /// Builds the reminder as a labeled user message (`kind:
    /// repeat-tool-reminder`, `form: notice`, summary `tool × count`).
    fn reminder_message(&self, text: &str, tool_name: &str, count: u64) -> Message {
        let id = self.reminder_ids.fetch_add(1, Ordering::SeqCst) + 1;
        let payload = serde_json::json!({
            "role": "user",
            "id": format!("repeat-tool-reminder-{id}"),
            "content": [{"type": "text", "text": text}],
            "source": {
                "kind": "repeat-tool-reminder",
                "form": "notice",
                "summary": format!("{tool_name} × {count}")
            }
        });
        // expect: failed only if the reminder shape left the wire contract
        serde_json::from_value(payload).expect("reminder shape stays a wire message")
    }

    /// Whether a tool participates in the chain (untracked calls neither
    /// count nor reset).
    fn tracked(&self, tool_name: &str) -> bool {
        if !self.include.is_empty() && !self.include.iter().any(|p| p.is_match(tool_name)) {
            return false;
        }
        !self.exclude.iter().any(|p| p.is_match(tool_name))
    }
}

/// Folds one reminder message onto a downstream decision (both decision
/// variants carry the context; `None` passes the decision through).
#[must_use]
pub fn fold_reminder(downstream: PostToolDecision, reminder: Option<Message>) -> PostToolDecision {
    let Some(reminder) = reminder else {
        return downstream;
    };
    match downstream {
        PostToolDecision::Block {
            feedback,
            additional_contexts,
        } => {
            let mut contexts = vec![reminder];
            contexts.extend(additional_contexts);
            PostToolDecision::Block {
                feedback,
                additional_contexts: contexts,
            }
        }
        PostToolDecision::Accept {
            content,
            value,
            additional_contexts,
        } => {
            let mut contexts = vec![reminder];
            contexts.extend(additional_contexts);
            PostToolDecision::Accept {
                content,
                value,
                additional_contexts: contexts,
            }
        }
    }
}

/// The detailed later-threshold reminder naming tool, run length, and the
/// canonical arguments (dsh `detailedReminder`).
#[must_use]
pub fn detailed_reminder(tool_name: &str, count: u64, canonical_arguments: &str) -> String {
    format!(
        "Repeated tool call detected:\n- tool: {tool_name}\n- consecutive_calls: {count}\n- arguments: {canonical_arguments}\nThe repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered."
    )
}

/// Canonical string form of a call's arguments: deep key-sort, then
/// stringify.
#[must_use]
pub fn canonicalize(value: &Value) -> String {
    serde_json::to_string(&sort_json_value(value)).unwrap_or_default()
}

/// Deep key-sort of one value so argument objects differing only in property
/// order canonicalize identically.
#[must_use]
pub fn sort_json_value(value: &Value) -> Value {
    match value {
        Value::Array(entries) => Value::Array(entries.iter().map(sort_json_value).collect()),
        Value::Object(record) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<&String> = record.keys().collect();
            keys.sort();
            for key in keys {
                sorted.insert(key.clone(), sort_json_value(&record[key]));
            }
            Value::Object(sorted)
        }
        other => other.clone(),
    }
}

/// Converts a validated integral preview cap to its integer form (the
/// truncating cast is guarded by the integrality check above).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value passed the integer-and-minimum validation"
)]
const fn integer_or_cap(value: f64) -> u64 {
    value as u64
}

/// Head-truncate the canonical arguments for quoting, marking how much was
/// omitted (dsh `previewArguments`).
#[must_use]
pub fn preview_arguments(canonical: &str, cap: u64) -> String {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "preview caps are far below usize::MAX"
    )]
    let cap = cap as usize;
    if canonical.len() <= cap {
        return canonical.to_owned();
    }
    format!(
        "{}… (+{} more chars)",
        &canonical[..cap.min(canonical.len())],
        canonical.len() - cap
    )
}

/// Compiles one `*`-wildcard pattern to an anchored regex (every other
/// metacharacter matches literally).
fn wildcard_to_regex(pattern: &str) -> Result<regex::Regex, regex::Error> {
    let mut rendered = String::from("^");
    for character in pattern.chars() {
        match character {
            '*' => rendered.push_str(".*"),
            '|' | '\\' | '{' | '}' | '(' | ')' | '[' | ']' | '^' | '$' | '+' | '?' | '.' => {
                rendered.push('\\');
                rendered.push(character);
            }
            other => rendered.push(other),
        }
    }
    rendered.push('$');
    regex::Regex::new(&rendered)
}

/// Validates thresholds per the fail-loud contract, sorted ascending.
fn validate_thresholds(values: &[Value]) -> Result<Vec<u64>, String> {
    if values.is_empty() {
        return Err("repeat-tool-reminder: `thresholds` must not be empty".to_owned());
    }
    let mut thresholds = Vec::new();
    for value in values {
        let rendered = match value {
            Value::Number(number) => js_number_to_string(number),
            other => other.to_string(),
        };
        match value.as_f64() {
            Some(number) if number.fract() == 0.0 && number >= 2.0 => {
                thresholds.push(integer_or_cap(number));
            }
            _ => {
                return Err(format!(
                    "repeat-tool-reminder: invalid threshold {rendered} — every threshold must be an integer >= 2"
                ));
            }
        }
    }
    if thresholds.len()
        != thresholds
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
    {
        return Err("repeat-tool-reminder: `thresholds` must not contain duplicates".to_owned());
    }
    thresholds.sort_unstable();
    Ok(thresholds)
}

/// Recovers one guard lock from poisoning.
fn recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One text content block (test fixture surface).
#[cfg(test)]
fn reminder_text_block(text: &str) -> Vec<fr_wire::ContentBlock> {
    fr_tools::types::text_content(text)
}

/// A minimal echo tool for the guard tests.
#[cfg(test)]
pub(crate) fn echo_tool(name: &str) -> Arc<dyn fr_tools::definition::ToolDefinition> {
    use fr_tools::definition::DefineTool;
    let tool = DefineTool {
        name: name.to_owned(),
        description: "echo".to_owned(),
        parameters: json!({"path": {"type": "string"}}),
        output_schema: json!({"type": "string"}),
        render: Arc::new(|_args: &serde_json::Value, value: &serde_json::Value| {
            crate::reminder_text_block(value.as_str().unwrap_or_default())
        }),
        presentation_meta: None,
        execute: Arc::new(|args: &serde_json::Value, _run| {
            Ok(args.get("path").cloned().unwrap_or_else(|| json!("x")))
        }),
        defer_loading: false,
        timeout_ms: None,
        concurrency_safe: None,
        project_content: None,
        finalize_content: None,
        present_call: None,
        present_result: None,
    };
    Arc::new(tool.build().expect("echo builds"))
}

#[cfg(test)]
mod tests {
    use fr_tools::registry::{ToolRegistry, ToolsEmitter};
    use fr_tools::types::{ToolExecutionInput, ToolPresentationMode};
    use fr_wire::KnownContentBlock;
    use serde_json::json;

    use super::*;

    /// A recording emitter capturing notifications.
    #[derive(Default)]
    struct NullEmitter;
    impl ToolsEmitter for NullEmitter {
        fn tools_change(&self) {}
        fn tools_result(&self, _exec: &ToolExecution, _result: &ToolExecutionResult) {}
    }

    fn exec_for(name: &str, arguments: Value) -> ToolExecutionInput {
        ToolExecutionInput {
            call_id: format!("call-{name}"),
            root_call_id: None,
            name: name.to_owned(),
            schema: None,
            arguments,
            agent: Some("agent-1".to_owned()),
            parent: None,
            signal: fr_tools::signal::ToolSignal::plain(fr_timeout::DeadlineSignal::new()),
        }
    }

    fn echo_text(content: &[fr_wire::ContentBlock]) -> String {
        content
            .iter()
            .map(|block| match block {
                fr_wire::ContentBlock::Known(KnownContentBlock::Text { text }) => text.clone(),
                _ => String::new(),
            })
            .collect()
    }

    fn reminder_text(message: &Message) -> String {
        match message {
            Message::Known(boxed) => match &**boxed {
                fr_wire::KnownMessage::User { content, .. } => echo_text(content),
                _ => String::new(),
            },
            Message::Other(_) => String::new(),
        }
    }

    #[test]
    fn thresholds_validate_fail_loud() {
        assert_eq!(
            RepeatGuard::try_new(&json!({"thresholds": []})).unwrap_err(),
            "repeat-tool-reminder: `thresholds` must not be empty"
        );
        assert_eq!(
            RepeatGuard::try_new(&json!({"thresholds": [3, 3.5]})).unwrap_err(),
            "repeat-tool-reminder: invalid threshold 3.5 — every threshold must be an integer >= 2"
        );
        assert_eq!(
            RepeatGuard::try_new(&json!({"thresholds": [3, 3]})).unwrap_err(),
            "repeat-tool-reminder: `thresholds` must not contain duplicates"
        );
        assert_eq!(
            RepeatGuard::try_new(&json!({"thresholds": [3, 5, 8], "argumentsPreviewChars": 0}))
                .unwrap_err(),
            "repeat-tool-reminder: invalid argumentsPreviewChars 0 — must be an integer >= 1"
        );
        let guard = RepeatGuard::try_new(&json!({"thresholds": [8, 3]})).expect("validates");
        assert_eq!(guard.config().thresholds, vec![3, 8]);
        assert_eq!(guard.config().arguments_preview_chars, 500);
    }

    #[test]
    fn gentle_then_detailed_escalation() {
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(crate::echo_tool("echo"))
            .expect("registers");
        let guard = RepeatGuard::try_new(&json!({})).expect("validates");
        let _listener = registry.on_post_execute(guard.listener());
        // Two repeats stay silent; the third is gentle; the fifth detailed.
        for expected_silent in 1..=2 {
            let result = registry.execute(&exec_for("echo", json!({"path": "a"})));
            assert!(
                result.additional_contexts().is_empty(),
                "call {expected_silent}"
            );
        }
        let third = registry.execute(&exec_for("echo", json!({"path": "a"})));
        let contexts = third.additional_contexts();
        assert_eq!(contexts.len(), 1);
        assert_eq!(reminder_text(&contexts[0]), GENTLE_REMINDER);
        registry.execute(&exec_for("echo", json!({"path": "a"})));
        let fifth = registry.execute(&exec_for("echo", json!({"path": "a"})));
        let contexts = fifth.additional_contexts();
        assert_eq!(contexts.len(), 1);
        assert_eq!(
            reminder_text(&contexts[0]),
            detailed_reminder("echo", 5, &json!({"path": "a"}).to_string())
        );
    }

    #[test]
    fn different_arguments_reset_and_order_is_canonical() {
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(crate::echo_tool("echo"))
            .expect("registers");
        let guard = RepeatGuard::try_new(&json!({})).expect("validates");
        let _listener = registry.on_post_execute(guard.listener());
        registry.execute(&exec_for("echo", json!({"a": 1, "b": 2})));
        // Key order differs; the chain continues.
        let second = registry.execute(&exec_for("echo", json!({"b": 2, "a": 1})));
        assert_eq!(second.additional_contexts(), [] as [fr_wire::Message; 0]);
        // Different arguments reset to a fresh chain.
        let changed = registry.execute(&exec_for("echo", json!({"a": 2, "b": 2})));
        assert_eq!(changed.additional_contexts(), [] as [fr_wire::Message; 0]);
        // An un-agented call never counts.
        let mut un_agented = exec_for("echo", json!({"a": 2, "b": 2}));
        un_agented.agent = None;
        registry.execute(&un_agented);
        // A user interjection resets the chain.
        guard.reset_chain("agent-1");
        for _ in 0..2 {
            let result = registry.execute(&exec_for("echo", json!({"a": 2, "b": 2})));
            assert_eq!(result.additional_contexts(), [] as [fr_wire::Message; 0]);
        }
    }

    #[test]
    fn include_exclude_patterns_are_wildcards() {
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(crate::echo_tool("echo"))
            .expect("registers");
        let _other = registry
            .register(crate::echo_tool("mcp_probe"))
            .expect("registers");
        let guard = RepeatGuard::try_new(&json!({"exclude": ["mcp_*"]})).expect("validates");
        let _listener = registry.on_post_execute(guard.listener());
        for _ in 0..3 {
            let result = registry.execute(&exec_for("mcp_probe", json!({})));
            assert_eq!(result.additional_contexts(), [] as [fr_wire::Message; 0]);
        }
        for _ in 0..2 {
            let silent = registry.execute(&exec_for("echo", json!({"x": 1})));
            assert_eq!(silent.additional_contexts(), [] as [fr_wire::Message; 0]);
        }
        let third = registry.execute(&exec_for("echo", json!({"x": 1})));
        assert_eq!(third.additional_contexts().len(), 1);

        let include_guard =
            RepeatGuard::try_new(&json!({"include": ["mcp_*"]})).expect("validates");
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(crate::echo_tool("echo"))
            .expect("registers");
        let _listener = registry.on_post_execute(include_guard.listener());
        for _ in 0..3 {
            let result = registry.execute(&exec_for("echo", json!({"x": 1})));
            assert_eq!(result.additional_contexts(), [] as [fr_wire::Message; 0]);
        }
    }

    #[test]
    fn preview_caps_the_detailed_quoting() {
        let long = "x".repeat(600);
        assert_eq!(
            preview_arguments(&long, 500),
            format!("{}… (+100 more chars)", "x".repeat(500))
        );
        assert_eq!(preview_arguments("short", 500), "short");
        // The chain key always compares the FULL canonical string: two
        // over-cap payloads with different tails are different chains.
        let first = preview_arguments(&long, 500);
        assert!(first.ends_with("(+100 more chars)"));
    }

    #[test]
    fn denied_calls_count_and_block_keeps_feedback() {
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(crate::echo_tool("echo"))
            .expect("registers");
        let guard = RepeatGuard::try_new(&json!({})).expect("validates");
        let _listener = registry.on_post_execute(guard.listener());
        // A pre-execute denial still flows through post-execute: the hammer
        // counts.
        let _deny = registry.on_pre_execute(std::sync::Arc::new(|_exec, _next| {
            fr_tools::types::PreToolDecision::Deny {
                reason: "no".to_owned(),
                info: None,
            }
        }));
        for _ in 0..2 {
            let result = registry.execute(&exec_for("echo", json!({"v": 1})));
            assert!(result.is_error());
            assert_eq!(result.additional_contexts(), [] as [fr_wire::Message; 0]);
        }
        let third = registry.execute(&exec_for("echo", json!({"v": 1})));
        assert!(third.is_error());
        assert_eq!(third.additional_contexts().len(), 1);
        assert_eq!(echo_text(third.content()), "Error: no");
        // A blocking downstream decision keeps its feedback and still rides
        // the reminder.
        let block_registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = block_registry
            .register(crate::echo_tool("echo"))
            .expect("registers");
        let block_guard = RepeatGuard::try_new(&json!({})).expect("validates");
        let _listener = block_registry.on_post_execute(block_guard.listener());
        let _listener =
            block_registry.on_post_execute(std::sync::Arc::new(|_exec, _result, _next| {
                PostToolDecision::Block {
                    feedback: fr_tools::types::text_content("stop"),
                    additional_contexts: Vec::new(),
                }
            }));
        for _ in 0..2 {
            block_registry.execute(&exec_for("echo", json!({"v": 1})));
        }
        let third = block_registry.execute(&exec_for("echo", json!({"v": 1})));
        assert!(third.is_error());
        assert_eq!(echo_text(third.content()), "stop");
        assert_eq!(third.additional_contexts().len(), 1);
    }
}
