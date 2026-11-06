// Rust guideline compliant 2026-11-06
//! The scoped structured-output runtime (dsh `subagent/src/structured.ts`
//! 1:1; the D1-B ruling): each requesting child owns its capture state,
//! so concurrent runs do not interact and settlement removes every
//! registration.
//!
//! The two-phase commit keys staged values by the execution identity:
//! the execute body only stages, and the authoritative post-pipeline
//! observation commits or discards. The monotonic guard denies every
//! later `structured_output` call once a capture exists.

use serde_json::{Value, json};

use crate::schema;

/// The model-facing tool name a structured child must call (dsh
/// `STRUCTURED_OUTPUT_TOOL`).
pub const STRUCTURED_OUTPUT_TOOL: &str = "structured_output";

/// The instruction registered as the child's scoped prompt section (dsh
/// `STRUCTURED_OUTPUT_INSTRUCTION`, verbatim).
pub const STRUCTURED_OUTPUT_INSTRUCTION: &str = "When you have your final answer, you MUST report \
it by calling the `structured_output` tool with arguments matching its parameter schema exactly. \
Do not finish with a plain text answer: only the tool call counts as your result.";

/// The tool description (dsh `schemaEntry.description`, verbatim).
pub const STRUCTURED_OUTPUT_DESCRIPTION: &str = "Report your final structured result. Call this \
exactly once, when your answer is complete; the arguments must match this tool's parameter schema \
exactly.";

/// The guard denial text (dsh `tools.guard` callback, verbatim).
#[must_use]
pub fn guard_denial() -> String {
    format!(
        "structured output already recorded: the run is complete, so `{STRUCTURED_OUTPUT_TOOL}` \
         is not executed"
    )
}

/// The completion precondition refusal (dsh `attachStructuredRuntime`
/// `canComplete` failure, verbatim).
pub const COMPLETION_REFUSAL: &str = "Wait for all delegated child tasks to finish and process \
pending messages before submitting structured output.";

/// The tool output schema (dsh `schemaEntry.output.schema`, verbatim).
#[must_use]
pub fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "recorded": { "type": "boolean", "const": true } },
        "required": ["recorded"],
        "additionalProperties": false,
    })
}

/// The rendered success text (dsh `schemaEntry.output.render`).
#[must_use]
pub fn render_recorded() -> Vec<Value> {
    vec![json!({ "type": "text", "text": "Structured output recorded." })]
}

/// One live structured run's capture state.
#[derive(Debug)]
pub struct StructuredRuntime {
    schema: Value,
    /// Validated values staged by the capture body, awaiting their own
    /// authoritative result notification (keyed by execution identity).
    staged: std::collections::HashMap<String, Value>,
    /// A successful nested capture waiting for its enclosing transport
    /// to commit (the PTC arm; keyed by the parent execution token).
    pending: Option<(String, Value)>,
    /// The committed value, exactly once.
    captured: Option<Value>,
}

impl StructuredRuntime {
    /// One live run over the asserted object-rooted schema.
    #[must_use]
    pub fn new(schema: Value) -> Self {
        Self {
            schema,
            staged: std::collections::HashMap::new(),
            pending: None,
            captured: None,
        }
    }

    /// The run's tool schema entry (the request schema as parameters).
    #[must_use]
    pub fn tool_schema(&self) -> Value {
        json!({
            "name": STRUCTURED_OUTPUT_TOOL,
            "description": STRUCTURED_OUTPUT_DESCRIPTION,
            "parameters": self.schema,
        })
    }

    /// Whether the run admits another input (dsh `acceptsInput`):
    /// staged or committed state closes the inbox.
    #[must_use]
    pub fn accepts_input(&self) -> bool {
        self.staged.is_empty() && self.pending.is_none() && self.captured.is_none()
    }

    /// Whether the run completed its capture.
    #[must_use]
    pub fn captured(&self) -> Option<&Value> {
        self.captured.as_ref()
    }

    /// The monotonic guard: a capture (or a staged/pending one) denies
    /// every later call.
    #[must_use]
    pub fn guard(&self) -> Option<String> {
        if self.captured.is_none() && self.pending.is_none() && self.staged.is_empty() {
            None
        } else {
            Some(guard_denial())
        }
    }

    /// Validates model arguments against the run schema (dsh
    /// `validateJsonSchemaValue` at the body boundary).
    #[must_use]
    pub fn validate(&self, args: &Value) -> Vec<String> {
        schema::validate(&self.schema, args)
    }

    /// Stages one validated capture (dsh `staged.set(exec, {value})`).
    pub fn stage(&mut self, execution: &str, value: Value) {
        self.staged.insert(execution.to_owned(), value);
    }

    /// Drops one staged entry without committing (the authoritative
    /// result rejected the call).
    pub fn discard(&mut self, execution: &str) {
        self.staged.remove(execution);
    }

    /// Observes one authoritative result notification (dsh `tools/result`
    /// listener): the structured tool's own result commits its staged
    /// entry — the direct arm immediately, the nested arm only after
    /// the enclosing transport commits.
    pub fn observe_result(
        &mut self,
        tool_name: &str,
        execution: &str,
        parent_execution: Option<&str>,
        is_error: bool,
    ) {
        if tool_name == STRUCTURED_OUTPUT_TOOL {
            if is_error {
                self.staged.remove(execution);
                return;
            }
            let Some(value) = self.staged.remove(execution) else {
                return;
            };
            match parent_execution {
                None => {
                    if self.captured.is_none() {
                        self.captured = Some(value);
                    }
                }
                Some(parent) => {
                    if self.captured.is_none() && self.pending.is_none() {
                        self.pending = Some((parent.to_owned(), value));
                    }
                }
            }
            return;
        }
        // A non-capture result may be the enclosing transport the nested
        // capture is waiting for.
        let Some((parent, _)) = &self.pending else {
            return;
        };
        if parent != execution || is_error {
            return;
        }
        let Some((_, value)) = self.pending.take() else {
            return;
        };
        if self.captured.is_none() {
            self.captured = Some(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        COMPLETION_REFUSAL, STRUCTURED_OUTPUT_INSTRUCTION, StructuredRuntime, guard_denial,
    };
    use serde_json::{Value, json};

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": { "answer": { "type": "integer" } },
            "required": ["answer"],
            "additionalProperties": false,
        })
    }

    #[test]
    fn the_instruction_is_the_dsh_verbatim_text() {
        assert!(STRUCTURED_OUTPUT_INSTRUCTION.starts_with(
            "When you have your final answer, you MUST report it by calling the \
                 `structured_output` tool"
        ));
        assert!(
            STRUCTURED_OUTPUT_INSTRUCTION.ends_with("only the tool call counts as your result.")
        );
        assert!(!COMPLETION_REFUSAL.is_empty());
        assert!(guard_denial().contains("structured output already recorded"));
    }

    #[test]
    fn the_direct_arm_commits_only_on_the_authoritative_success() {
        let mut runtime = StructuredRuntime::new(schema());
        assert!(runtime.validate(&json!({ "answer": 42 })).is_empty());
        let violations = runtime.validate(&json!({ "answer": "forty-two" }));
        assert_eq!(violations, ["\"answer\" must be an integer"]);

        // Stage the capture; the result is not observed yet.
        runtime.stage("exec-1", json!({ "answer": 42 }));
        assert!(runtime.captured().is_none());
        assert!(!runtime.accepts_input());
        assert!(runtime.guard().is_some());
        // The authoritative error discards the stage and reopens nothing.
        runtime.observe_result("structured_output", "exec-1", None, true);
        assert!(runtime.captured().is_none());
        assert!(runtime.accepts_input());

        // The second attempt commits on the authoritative success.
        runtime.stage("exec-2", json!({ "answer": 7 }));
        runtime.observe_result("structured_output", "exec-2", None, false);
        assert_eq!(runtime.captured(), Some(&json!({ "answer": 7 })));
        assert!(!runtime.accepts_input());
        assert!(runtime.guard().is_some());
    }

    #[test]
    fn the_nested_arm_waits_for_the_enclosing_transport() {
        let mut runtime = StructuredRuntime::new(schema());
        runtime.stage("inner", json!({ "answer": 1 }));
        runtime.observe_result("structured_output", "inner", Some("outer"), false);
        // Staged into the pending slot: not captured yet.
        assert!(runtime.captured().is_none());
        assert!(!runtime.accepts_input());
        // The enclosing run_code result commits it.
        runtime.observe_result("run_code", "outer", None, false);
        assert_eq!(runtime.captured(), Some(&json!({ "answer": 1 })));
        // A failed enclosing transport drops the pending capture.
        let mut runtime = StructuredRuntime::new(schema());
        runtime.stage("inner", json!({ "answer": 2 }));
        runtime.observe_result("structured_output", "inner", Some("outer"), false);
        runtime.observe_result("run_code", "outer", None, true);
        assert!(runtime.captured().is_none());
    }

    #[test]
    fn the_guard_denies_later_calls_monotonically() {
        let mut runtime = StructuredRuntime::new(schema());
        assert!(runtime.guard().is_none());
        runtime.stage("exec", json!({ "answer": 1 }));
        assert_eq!(runtime.guard().as_deref(), Some(guard_denial().as_str()));
        runtime.observe_result("structured_output", "exec", None, false);
        assert!(runtime.guard().is_some());
    }
}
