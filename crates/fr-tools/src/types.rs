// Rust guideline compliant 2026-10-28
//! Pipeline value types: identities, structured failures, stage decisions,
//! and the execution result family (dsh `core/tools/src/index.ts` type
//! surface, wire-verbatim field spellings).
//!
//! Value-semantics absences (slice-7 judgment): `deepFreeze`/
//! `snapshotJsonValue` lossless-JSON checks are structurally satisfied by
//! owned `serde_json` values, so the `TypeError` texts guarding them in dsh
//! (`tool execution arguments must be losslessly JSON-serializable`,
//! `tool result must be losslessly JSON-serializable`) have no reachable
//! Rust path. The dsh `symbol` execution token becomes a monotonically
//! minted `u64`; canonical-result identity (a `WeakMap` in dsh) becomes the
//! the registry canonical-versus-authored distinction distinction at the around-dispatch boundary.

use fr_wire::{ContentBlock, Message};
use std::sync::Arc;

use crate::signal::ToolSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The reserved PTC mode presentation transport name (dsh `RUN_CODE_NAME`).
pub const RUN_CODE_NAME: &str = "run_code";

/// Canonical error code for cancellation after a tool body was invoked.
pub const TOOL_ABORTED: &str = "ABORTED";

/// Canonical error code for cancellation before a tool body was invoked.
pub const TOOL_ABORTED_BEFORE_DISPATCH: &str = "ABORTED_BEFORE_DISPATCH";

/// Error code of the unknown-tool failure (dsh `ToolNotFoundError`).
pub const UNKNOWN_TOOL: &str = "UNKNOWN_TOOL";

/// Error code of a body or policy value violating its declared output.
pub const INVALID_TOOL_OUTPUT: &str = "INVALID_TOOL_OUTPUT";

/// One model-issued tool-call identity.
pub type ToolCallId = String;

/// Opaque same-process execution identity; minted by the registry.
pub type ToolExecutionToken = u64;

/// The agent a call runs on behalf of (an opaque agent identity; M1 has a
/// single unnamed scope, so calls usually carry `None`).
pub type AgentRef = String;

/// Structured error metadata for a failed tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolErrorInfo {
    /// Error class name (dsh `HarnessError.name`).
    pub name: String,
    /// Harness error code.
    pub code: String,
    /// Optional raw user-facing detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Canonical failure detail of one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolFailure {
    /// Human-readable failure message without the `Error: ` envelope.
    pub message: String,
    /// Internal error class/code used by policy and durable diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<ToolErrorInfo>,
}

/// One tool error raised by a body or the registry itself: the structured
/// failure plus the code the routing layer reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    /// The harness error code (dsh `HarnessError.code`).
    pub code: &'static str,
    /// The structured failure carried into the result.
    pub failure: ToolFailure,
}

impl ToolError {
    /// Builds one coded error.
    #[must_use]
    pub fn new(code: &'static str, name: &str, message: &str) -> Self {
        Self {
            code,
            failure: ToolFailure {
                message: message.to_owned(),
                info: Some(ToolErrorInfo {
                    name: name.to_owned(),
                    code: code.to_owned(),
                    reason: None,
                }),
            },
        }
    }

    /// Builds one uncoded error (a plain thrown value in dsh).
    #[must_use]
    pub fn plain(message: &str) -> Self {
        Self {
            code: UNKNOWN_TOOL,
            failure: ToolFailure {
                message: message.to_owned(),
                info: None,
            },
        }
    }
}

/// One model-facing tool schema (the allowlisted projection fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSchema {
    /// Tool name.
    pub name: String,
    /// Human-readable description sent to the model.
    pub description: String,
    /// Raw JSON Schema parameters object.
    pub parameters: Value,
    /// Requests deferred loading of the tool definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
}

/// Scheduling mode for one pending call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// May overlap with siblings.
    Parallel,
    /// Runs alone and forms an ordering barrier.
    Exclusive,
}

/// The completed outcome handed to `presentResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    /// The final model-facing content (or the rendered error text).
    pub content: Vec<ContentBlock>,
    /// Whether the call failed.
    pub is_error: bool,
    /// The tool-private presentation payload, when projected.
    pub meta: Option<Value>,
}

/// A successful canonical tool execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionSuccess {
    /// Execution-local canonical value; omitted from durable events.
    pub value: Value,
    /// The model-facing rendered content.
    pub content: Vec<ContentBlock>,
    /// The tool-private presentation payload, when projected.
    pub meta: Option<Value>,
    /// Contexts the loop appends after `tool/result`.
    pub additional_contexts: Vec<Message>,
    /// The agent loop stops after committing this successful result batch.
    pub concludes_turn: bool,
}

/// A failed canonical tool execution; failures never carry a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionFailure {
    /// The structured failure.
    pub error: ToolFailure,
    /// The model-facing error content.
    pub content: Vec<ContentBlock>,
    /// The tool-private presentation payload, when projected.
    pub meta: Option<Value>,
    /// Contexts the loop appends after `tool/result`.
    pub additional_contexts: Vec<Message>,
}

/// The discriminated, execution-local outcome of one tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum ToolExecutionResult {
    /// The call succeeded through its output contract.
    Success(ToolExecutionSuccess),
    /// The call failed.
    Failure(ToolExecutionFailure),
}

impl ToolExecutionResult {
    /// Whether the call failed.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self, Self::Failure(_))
    }

    /// The model-facing content of either outcome.
    #[must_use]
    pub fn content(&self) -> &[ContentBlock] {
        match self {
            Self::Success(success) => &success.content,
            Self::Failure(failure) => &failure.content,
        }
    }

    /// The presentation payload of either outcome, when projected.
    #[must_use]
    pub const fn meta(&self) -> Option<&Value> {
        match self {
            Self::Success(success) => success.meta.as_ref(),
            Self::Failure(failure) => failure.meta.as_ref(),
        }
    }

    /// The appended contexts of either outcome.
    #[must_use]
    pub fn additional_contexts(&self) -> &[Message] {
        match self {
            Self::Success(success) => &success.additional_contexts,
            Self::Failure(failure) => &failure.additional_contexts,
        }
    }
}

/// Pre-dispatch decision of the `tools/pre-execute` waterfall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum PreToolDecision {
    /// Run the call.
    Allow,
    /// Materialize the reason (and optional structured identity) as an error
    /// result.
    Deny {
        /// The model-facing denial reason.
        reason: String,
        /// Optional structured error identity.
        info: Option<ToolErrorInfo>,
    },
    /// Select the canonical cancellation result without a policy denial.
    Cancel,
    /// Resolve through the approval seam; `allowed-once` allows, everything
    /// else denies.
    Ask {
        /// The audited approval reason.
        reason: Option<String>,
        /// The localized prompt text, when any.
        display_reason: Option<String>,
    },
}

/// Localized display text of an ask decision (dsh `displayReason`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayReason {
    /// The English prompt text.
    pub en: String,
}

/// Post-dispatch decision of the `tools/post-execute` waterfall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum PostToolDecision {
    /// Accept the result, optionally replacing one projection or attaching
    /// context. Setting both `content` and `value` rejects with the dsh
    /// `TypeError` at apply time.
    Accept {
        /// Replacement content, or `None` to keep it.
        content: Option<Vec<ContentBlock>>,
        /// Replacement canonical value (re-rendered through the output
        /// contract), or `None` to keep it.
        value: Option<Value>,
        /// Contexts attached for the next request.
        additional_contexts: Vec<Message>,
    },
    /// Turn the call into an error result carrying the corrective feedback.
    Block {
        /// The corrective feedback rendered as the error content.
        feedback: Vec<ContentBlock>,
        /// Contexts attached for the next request.
        additional_contexts: Vec<Message>,
    },
}

/// Caller-supplied description of one tool call.
#[derive(Debug, Clone)]
pub struct ToolExecutionInput {
    /// Model-issued call identity.
    pub call_id: ToolCallId,
    /// Root model-requested call; `None` for a root execution.
    pub root_call_id: Option<ToolCallId>,
    /// The tool name as requested.
    pub name: String,
    /// Binding-time tool schema for a PTC inner call (never logged).
    pub schema: Option<ToolSchema>,
    /// Parsed arguments (lossless JSON by construction).
    pub arguments: Value,
    /// The agent on whose behalf the call runs.
    pub agent: Option<AgentRef>,
    /// Token of the enclosing transport execution, when nested.
    pub parent: Option<ToolExecutionToken>,
    /// Caller-owned cancellation for this invocation.
    pub signal: Arc<ToolSignal>,
}

/// One pending tool call inside the registry pipeline: the input plus the
/// registry-assigned identity.
#[derive(Debug, Clone)]
pub struct ToolExecution {
    /// Registry-assigned identity.
    pub token: ToolExecutionToken,
    /// Root model-requested call, resolved for every execution.
    pub root_call_id: ToolCallId,
    /// Model-issued call identity.
    pub call_id: ToolCallId,
    /// The tool name as requested.
    pub name: String,
    /// Binding-time schema, when a PTC inner call carried one.
    pub schema: Option<ToolSchema>,
    /// Frozen parsed arguments.
    pub arguments: Value,
    /// The agent on whose behalf the call runs.
    pub agent: Option<AgentRef>,
    /// Token of the enclosing transport execution, when nested.
    pub parent: Option<ToolExecutionToken>,
    /// Caller-owned cancellation for this invocation.
    pub signal: Arc<ToolSignal>,
}

impl ToolExecution {
    /// Whether the call is a transport sub-dispatch rather than a
    /// model-direct call.
    #[must_use]
    pub const fn nested(&self) -> bool {
        self.parent.is_some()
    }
}

/// How the registry presents its tools to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolPresentationMode {
    /// Send every visible schema.
    #[default]
    Native,
    /// Send only the reserved transport plus a generated SDK prompt.
    Ptc,
}

/// A monotonic execution guard evaluated after every `tools/pre-execute`
/// listener and before the tool body; a returned reason denies the call.
pub type ToolGuard = std::sync::Arc<dyn Fn(&ToolExecution) -> Option<String> + Send + Sync>;

/// The unknown-tool failure (dsh `ToolNotFoundError`).
#[must_use]
pub fn unknown_tool_error(tool_name: &str, reachable_from: Option<&str>) -> ToolError {
    let message = reachable_from.map_or_else(
        || format!("unknown tool \"{tool_name}\""),
        |route| format!("unknown tool \"{tool_name}\": {route}"),
    );
    ToolError::new(UNKNOWN_TOOL, "ToolNotFoundError", &message)
}

/// The invalid-output failure (dsh `ToolOutputError`).
#[must_use]
pub fn invalid_output_error(tool_name: &str, violations: &[String]) -> ToolError {
    ToolError::new(
        INVALID_TOOL_OUTPUT,
        "ToolOutputError",
        &format!(
            "tool \"{tool_name}\" returned invalid output: {}",
            violations.join("; ")
        ),
    )
}

/// The canonical result when cancellation supersedes success after body
/// invocation (dsh `toolAbortedResult`).
#[must_use]
pub fn tool_aborted_result(prior: Option<&ToolExecutionResult>) -> ToolExecutionResult {
    ToolExecutionResult::Failure(ToolExecutionFailure {
        content: text_content("Error: tool call aborted"),
        error: ToolFailure {
            message: "tool call aborted".to_owned(),
            info: Some(ToolErrorInfo {
                name: "AbortError".to_owned(),
                code: TOOL_ABORTED.to_owned(),
                reason: None,
            }),
        },
        meta: None,
        additional_contexts: prior
            .map(ToolExecutionResult::additional_contexts_to_owned)
            .unwrap_or_default(),
    })
}

/// The canonical result when cancellation prevents body invocation (dsh
/// `toolAbortedBeforeDispatchResult`).
#[must_use]
pub fn tool_aborted_before_dispatch_result(
    prior: Option<&ToolExecutionResult>,
) -> ToolExecutionResult {
    ToolExecutionResult::Failure(ToolExecutionFailure {
        content: text_content("Error: tool call aborted before dispatch"),
        error: ToolFailure {
            message: "tool call aborted before dispatch".to_owned(),
            info: Some(ToolErrorInfo {
                name: "AbortError".to_owned(),
                code: TOOL_ABORTED_BEFORE_DISPATCH.to_owned(),
                reason: None,
            }),
        },
        meta: None,
        additional_contexts: prior
            .map(ToolExecutionResult::additional_contexts_to_owned)
            .unwrap_or_default(),
    })
}

/// One text content block.
#[must_use]
pub fn text_content(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Known(fr_wire::KnownContentBlock::Text {
        text: text.to_owned(),
    })]
}

impl ToolExecutionResult {
    /// Clones the appended contexts out of either outcome.
    #[must_use]
    pub fn additional_contexts_to_owned(&self) -> Vec<Message> {
        self.additional_contexts().to_vec()
    }
}
