// Rust guideline compliant 2026-11-06
//! The seam's shared vocabulary: the coded error, the stop-reason set,
//! the verbatim model-facing wording family, and the delegation-policy
//! capture (dsh `subagent/src/error.ts`, `types.ts`
//! `SubagentStopReasonMap`, `child-agent.ts`, and
//! `continuation-messages.ts` 1:1 ports).
//!
//! Every user-visible string in this module is verbatim from the dsh
//! source; the fr carrier changes only how the values travel (JSON
//! error bodies over the dynamic channel instead of thrown Error
//! objects).

use serde_json::{Value, json};

/// Why a subagent run ended (dsh `SubagentStopReasonMap`; the known
/// cases mirror the harness turn-end vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    /// The child finished its turn normally.
    Completed,
    /// Cancelled through the request signal or disposal.
    Aborted,
    /// Model or transport failure.
    Error,
    /// The child hit its token ceiling before finishing.
    MaxTokens,
    /// The child declined the task.
    Refusal,
}

impl StopReason {
    /// The wire spelling (the dsh kebab-case union member).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::Error => "error",
            Self::MaxTokens => "max-tokens",
            Self::Refusal => "refusal",
        }
    }

    /// Parses one wire spelling; an unknown backend-added variant reads
    /// as `Error` rather than reporting failed work as completed (the
    /// dsh `default` arm).
    #[must_use]
    pub fn from_wire(value: &str) -> Self {
        match value {
            "completed" => Self::Completed,
            "aborted" => Self::Aborted,
            "error" => Self::Error,
            "max-tokens" => Self::MaxTokens,
            "refusal" => Self::Refusal,
            _ => Self::Error,
        }
    }
}

/// The coded seam failure (dsh `SubagentError`): the `code` routes the
/// wire refusal, the message stays verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentError {
    /// The routing code (dsh `SubagentError.code`).
    pub code: &'static str,
    /// The verbatim user-visible message.
    pub message: String,
}

impl SubagentError {
    /// One coded failure.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The duplicate-child refusal (dsh `DUPLICATE_CHILD`).
    #[must_use]
    pub fn duplicate_child(child_id: &str) -> Self {
        Self::new(
            "DUPLICATE_CHILD",
            format!("subagent \"{child_id}\" already exists"),
        )
    }

    /// The depth-cap refusal (dsh `SubagentDepthError`).
    #[must_use]
    pub fn depth(attempted: u64, max_depth: u64) -> Self {
        Self::new(
            "DEPTH_LIMIT",
            format!("subagent depth {attempted} exceeds maxDepth {max_depth}"),
        )
    }

    /// The activation-pool refusal (dsh `ACTIVATION_LIMIT_REACHED`).
    #[must_use]
    pub fn activation_limit(capacity: u64) -> Self {
        Self::new(
            "ACTIVATION_LIMIT_REACHED",
            format!(
                "subagent limit reached (active child limit: {capacity}); wait for an existing \
                 child to finish or complete this work with the current agents"
            ),
        )
    }

    /// The unauthorized-address refusal (dsh `UNAUTHORIZED`).
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new("UNAUTHORIZED", message)
    }

    /// The structured JSON error body (the fr carrier for the thrown
    /// Error over the dynamic channel).
    #[must_use]
    pub fn body(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }
}

impl std::fmt::Display for SubagentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SubagentError {}

/// The terminal outcome of one subagent run (dsh `SubagentResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentResult {
    /// The selected final assistant output (possibly empty).
    pub output: Vec<Value>,
    /// The committed structured value, when one was captured.
    pub structured: Option<Value>,
    /// Why the run ended.
    pub stop_reason: StopReason,
}

/// The model-facing delegation-scope statement for every in-process
/// child (dsh `SUBAGENT_DELEGATION_CONTEXT`, verbatim).
pub const SUBAGENT_DELEGATION_CONTEXT: &str = "You are a delegated subagent: your permission \
scope was fixed when you were started and cannot be widened from inside this session — \
operations that require approval are rejected automatically. When the task needs access \
beyond that scope, do not retry the denied operation; state the limitation in your reply so \
the delegating agent can handle it.";

/// The adjacent-Agent return guidance appended to a continuable child's
/// initial task (dsh `withContinuableReturnGuidance`, verbatim).
#[must_use]
pub fn continuable_return_guidance(parent_id: &str, prompt: &[Value]) -> Vec<Value> {
    let encoded = serde_json::to_string(parent_id).unwrap_or_else(|_| "\"\"".to_owned());
    let mut blocks = prompt.to_vec();
    blocks.push(json!({
        "type": "text",
        "text": format!(
            "Your parent agent id is {encoded}. Before you finish, send your result to that \
             agent with send_message({{ agent_id: {encoded}, message: \"<self-contained \
             result>\" }}). The parent shares your workspace but does not automatically \
             receive your transcript, tool output, or reasoning. Send earlier messages as \
             well when a finding changes what the parent should do next; sending a message \
             does not end your turn."
        ),
    }));
    blocks
}

/// One line telling a parent that a background child is finished and
/// why (dsh `settlementSummary`, verbatim).
#[must_use]
pub fn settlement_summary(child_id: &str, stop_reason: StopReason, continuable: bool) -> String {
    let subject = format!("Background subagent {child_id}");
    match stop_reason {
        StopReason::Completed => {
            if continuable {
                format!("{subject} finished and will do no further work unless you send it more.")
            } else {
                format!("{subject} finished. It cannot receive follow-up messages.")
            }
        }
        StopReason::Aborted => format!("{subject} was stopped before it finished."),
        StopReason::MaxTokens => format!("{subject} ran out of room before it finished."),
        StopReason::Refusal => format!("{subject} declined the task."),
        StopReason::Error => format!("{subject} failed before it finished."),
    }
}

/// The runtime-owned settlement notice content blocks (dsh
/// `createSettlementMessage`): the summary line, the closing-message
/// pair, and the optional structured-result line.
#[must_use]
pub fn settlement_notice_blocks(
    child_id: &str,
    result: &SubagentResult,
    continuable: bool,
) -> Vec<Value> {
    let summary = settlement_summary(child_id, result.stop_reason, continuable);
    let closing: Vec<Value> = result
        .output
        .iter()
        .filter(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty())
        })
        .cloned()
        .collect();
    let mut blocks = vec![json!({ "type": "text", "text": summary })];
    if closing.is_empty() {
        blocks.push(json!({ "type": "text", "text": "It left no closing message." }));
    } else {
        blocks.push(json!({ "type": "text", "text": "Its closing message:" }));
        blocks.extend(closing);
    }
    if let Some(structured) = &result.structured {
        blocks.push(json!({
            "type": "text",
            "text": format!("Structured result: {}", serde_json::to_string(structured).unwrap_or_default()),
        }));
    }
    blocks
}

/// The durable `subagent-settled` source record (dsh
/// `SubagentSettledMessageSource`).
#[must_use]
pub fn settlement_source(summary: &str, child_id: &str) -> Value {
    json!({
        "kind": crate::wire::SOURCE_SUBAGENT_SETTLED,
        "form": "notice",
        "summary": summary,
        "senderSessionId": child_id,
    })
}

/// The model-visible and durable representation of one adjacent-Agent
/// message (dsh `createAgentMessage`): the `Agent <id> sent a message: `
/// wrapping prefix plus the relay source.
#[must_use]
pub fn agent_message_blocks(sender_id: &str, content: &[Value]) -> (Vec<Value>, Value) {
    let mut blocks = vec![json!({
        "type": "text",
        "text": format!("Agent {sender_id} sent a message: "),
    })];
    blocks.extend(content.iter().cloned());
    let source = json!({
        "kind": crate::wire::SOURCE_AGENT_MESSAGE,
        "form": "relay",
        "senderSessionId": sender_id,
    });
    (blocks, source)
}

/// The delegation-policy facts captured at the delegation boundary (dsh
/// `DelegatedPolicyOverrides`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegatedPolicyOverrides {
    /// The parent's current preset identity when it runs in Auto or
    /// Full access; the child keeps it under the pinned never policy.
    pub permission_preset: Option<String>,
    /// The parent session's explicit sandbox-mode override.
    pub sandbox_mode: Option<String>,
    /// `'never'` whenever the approval capability is composed.
    pub approval_policy: Option<&'static str>,
}

/// The event rows the captured policy appends to the child's own log
/// (dsh `appendDelegatedPolicyOverrides`, verbatim event order).
#[must_use]
pub fn delegated_policy_events(overrides: &DelegatedPolicyOverrides) -> Vec<(&'static str, Value)> {
    let mut events = Vec::new();
    if let Some(mode) = &overrides.sandbox_mode {
        events.push((
            crate::wire::SANDBOX_MODE_EVENT,
            json!({ "mode": mode, "source": "delegation" }),
        ));
    }
    if let Some(policy) = overrides.approval_policy {
        events.push((
            crate::wire::APPROVAL_POLICY_EVENT,
            json!({ "policy": policy, "source": "delegation" }),
        ));
    }
    if let Some(preset) = &overrides.permission_preset {
        events.push((
            crate::wire::PERMISSION_PRESET_EVENT,
            json!({ "preset": preset }),
        ));
    }
    events
}

/// Whether a content block list carries an image part (dsh
/// `contentHasImage`).
#[must_use]
pub fn content_has_image(content: &[Value]) -> bool {
    content
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("image"))
}

#[cfg(test)]
mod tests {
    use super::{
        DelegatedPolicyOverrides, SUBAGENT_DELEGATION_CONTEXT, StopReason, agent_message_blocks,
        continuable_return_guidance, delegated_policy_events, settlement_notice_blocks,
        settlement_summary,
    };
    use serde_json::{Value, json};

    #[test]
    fn the_delegation_context_is_the_dsh_verbatim_text() {
        assert!(SUBAGENT_DELEGATION_CONTEXT.starts_with(
            "You are a delegated subagent: your permission scope was fixed when you were \
                 started and cannot be widened"
        ));
        assert!(SUBAGENT_DELEGATION_CONTEXT.ends_with("can handle it."));
    }

    #[test]
    fn the_summary_family_matches_every_stop_reason_verbatim() {
        let child = "child-1";
        assert_eq!(
            settlement_summary(child, StopReason::Completed, true),
            "Background subagent child-1 finished and will do no further work unless you send \
             it more."
        );
        assert_eq!(
            settlement_summary(child, StopReason::Completed, false),
            "Background subagent child-1 finished. It cannot receive follow-up messages."
        );
        assert_eq!(
            settlement_summary(child, StopReason::Aborted, true),
            "Background subagent child-1 was stopped before it finished."
        );
        assert_eq!(
            settlement_summary(child, StopReason::MaxTokens, true),
            "Background subagent child-1 ran out of room before it finished."
        );
        assert_eq!(
            settlement_summary(child, StopReason::Refusal, true),
            "Background subagent child-1 declined the task."
        );
        assert_eq!(
            settlement_summary(child, StopReason::Error, true),
            "Background subagent child-1 failed before it finished."
        );
    }

    #[test]
    fn the_notice_carries_the_empty_closing_and_structured_lines() {
        let result = super::SubagentResult {
            output: Vec::new(),
            structured: Some(json!({ "answer": 42 })),
            stop_reason: StopReason::Completed,
        };
        let blocks = settlement_notice_blocks("child-1", &result, true);
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect();
        assert_eq!(
            texts,
            [
                "Background subagent child-1 finished and will do no further work unless you \
                 send it more.",
                "It left no closing message.",
                "Structured result: {\"answer\":42}",
            ]
        );
    }

    #[test]
    fn the_notice_prefers_the_closing_message_line() {
        let result = super::SubagentResult {
            output: vec![json!({ "type": "text", "text": "done" })],
            structured: None,
            stop_reason: StopReason::Refusal,
        };
        let blocks = settlement_notice_blocks("child-9", &result, false);
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect();
        assert_eq!(
            texts,
            [
                "Background subagent child-9 declined the task.",
                "Its closing message:",
                "done",
            ]
        );
    }

    #[test]
    fn the_return_guidance_names_the_parent_and_send_message() {
        let blocks =
            continuable_return_guidance("parent-1", &[json!({"type":"text","text":"task"})]);
        assert_eq!(blocks.len(), 2);
        let text = blocks[1]
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            text.contains("Your parent agent id is \"parent-1\"."),
            "{text}"
        );
        assert!(
            text.contains("send_message({ agent_id: \"parent-1\", message:"),
            "{text}"
        );
        assert!(
            text.contains("sending a message does not end your turn."),
            "{text}"
        );
    }

    #[test]
    fn the_agent_message_wraps_and_tags_the_relay_source() {
        let (blocks, source) =
            agent_message_blocks("child-2", &[json!({"type":"text","text":"hello"})]);
        assert_eq!(
            blocks[0],
            json!({ "type": "text", "text": "Agent child-2 sent a message: " })
        );
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            source,
            json!({
                "kind": "agent-message",
                "form": "relay",
                "senderSessionId": "child-2",
            })
        );
    }

    #[test]
    fn the_delegated_policy_events_follow_the_dsh_order() {
        let overrides = DelegatedPolicyOverrides {
            permission_preset: Some("auto".to_owned()),
            sandbox_mode: Some("workspace-write".to_owned()),
            approval_policy: Some("never"),
        };
        let events = delegated_policy_events(&overrides);
        let kinds: Vec<&str> = events.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            ["sandbox/mode", "approval/policy", "permission/preset"]
        );
        assert_eq!(
            events[0].1,
            json!({ "mode": "workspace-write", "source": "delegation" })
        );
        assert_eq!(
            events[1].1,
            json!({ "policy": "never", "source": "delegation" })
        );
        assert_eq!(events[2].1, json!({ "preset": "auto" }));
    }
}
