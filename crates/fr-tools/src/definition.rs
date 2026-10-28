// Rust guideline compliant 2026-10-28
//! Tool definitions: the registered contract (dsh `ToolDefinition` /
//! `ToolOutputDefinition` / `defineTool` of `core/tools/src/index.ts` and
//! `schema.ts`).
//!
//! A definition carries the model-facing schema fields, the canonical output
//! declaration (raw schema + pure render/presentation projections), the
//! execution body, and the optional policy hooks (`projectContent`,
//! `finalizeContent`, `presentCall`, `presentResult`,
//! `isConcurrencySafe`). The [`DefineTool`] builder is the `defineTool`
//! counterpart: it compiles the author-facing parameter/output specs, hard-
//! validates arguments before the body (`ToolArgsError`), and soft-wraps the
//! replay presenters (a mismatch falls back to the generic rendering rather
//! than failing).

use std::sync::Arc;

use fr_wire::{ContentBlock, Message};
use serde_json::Value;

use crate::json_schema::{assert_supported_json_schema, validate_json_schema_value};
use crate::presentation::{ToolCallView, ToolResultView};
use crate::schema::{parameter_schema_spec_to_json_schema, value_schema_spec_to_json_schema};
use crate::signal::ToolSignal;
use crate::types::{ToolError, ToolExecution, ToolExecutionResult, ToolResult};

/// One failed [`DefineTool::build`] (schema compilation or the timeout
/// validation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefineToolError {
    /// The parameter spec fell outside the author DSL or raw subset.
    Parameters(crate::json_schema::JsonSchemaError),
    /// The output spec fell outside the author DSL or raw subset.
    Output(crate::json_schema::JsonSchemaError),
    /// The timeout budget failed the positive-finite check.
    TimeoutMs(String),
}

impl std::fmt::Display for DefineToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parameters(error) | Self::Output(error) => write!(f, "{error}"),
            Self::TimeoutMs(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DefineToolError {}

/// Tool-owned canonical output contract used after the body returns a value.
pub struct ToolOutput {
    /// Raw supported JSON Schema enforced against every successful value.
    pub schema: Value,
    /// Pure projection from validated arguments and value to model content.
    pub render: RenderProjector,
    /// Pure replayable presentation projection.
    pub presentation_meta: Option<PresentationProjector>,
}

impl std::fmt::Debug for ToolOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolOutput")
            .field("schema", &self.schema)
            .field("render", &(type_name_of(&self.render)))
            .field(
                "presentation_meta",
                &self.presentation_meta.as_ref().map(|m| type_name_of(m)),
            )
            .finish()
    }
}

/// The static name of one closure, for debug output.
fn type_name_of<T>(_: &T) -> &'static str {
    std::any::type_name::<T>()
}

/// Pure render projection of a validated value.
pub type RenderProjector = Arc<dyn Fn(&Value, &Value) -> Vec<ContentBlock> + Send + Sync>;
/// Pure replayable presentation projection.
pub type PresentationProjector = Arc<dyn Fn(&Value, &Value) -> Value + Send + Sync>;
/// Pure concurrency classifier.
pub type ConcurrencyClassifier = Arc<dyn Fn(&Value) -> bool + Send + Sync>;
/// Execution-prepared content projection.
pub type ContentProjector =
    Arc<dyn Fn(&ToolExecution, &ToolExecutionResult) -> Option<Vec<ContentBlock>> + Send + Sync>;
/// Pending-state presenter.
pub type PresentCallFn = Arc<dyn Fn(&Value) -> Option<ToolCallView> + Send + Sync>;
/// Completed-state presenter.
pub type PresentResultFn = Arc<dyn Fn(&Value, &ToolResult) -> Option<ToolResultView> + Send + Sync>;

/// The run context handed to a tool body: the execution plus the
/// context-deferral and turn-conclusion channels (dsh `ToolRunContext`).
#[derive(Debug)]
pub struct ToolRunContext<'a> {
    /// The execution the body runs for.
    pub exec: &'a ToolExecution,
    /// The current cancellation signal (fused by the registry).
    pub signal: Arc<ToolSignal>,
    deferred_contexts: Vec<Message>,
    concludes_turn: bool,
}

impl<'a> ToolRunContext<'a> {
    /// Starts one run context over an execution.
    #[must_use]
    pub const fn new(exec: &'a ToolExecution, signal: Arc<ToolSignal>) -> Self {
        Self {
            exec,
            signal,
            deferred_contexts: Vec::new(),
            concludes_turn: false,
        }
    }

    /// Defer one context until this tool's final result reaches the agent
    /// loop; contexts are emitted in call order.
    pub fn defer_context(&mut self, context: Message) {
        self.deferred_contexts.push(context);
    }

    /// Mark a successful final result as terminal for the current agent
    /// turn.
    pub const fn conclude_turn(&mut self) {
        self.concludes_turn = true;
    }

    /// Takes the deferred contexts (registry-internal).
    #[must_use]
    pub fn take_deferred_contexts(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.deferred_contexts)
    }

    /// Whether the body declared the turn complete (registry-internal).
    #[must_use]
    pub const fn concluded_turn(&self) -> bool {
        self.concludes_turn
    }
}

/// The body signature: validated arguments plus the run context.
pub type ToolBody =
    Arc<dyn Fn(&Value, &mut ToolRunContext<'_>) -> Result<Value, ToolError> + Send + Sync>;

/// A registered tool: its schema plus the execution function.
pub trait ToolDefinition: Send + Sync {
    /// Tool name (unique within its layer).
    fn name(&self) -> &str;
    /// Human-readable description sent to the model.
    fn description(&self) -> &str;
    /// Raw JSON Schema parameters object (asserted subset at registration).
    fn parameters(&self) -> &Value;
    /// The canonical output declaration.
    fn output(&self) -> &ToolOutput;

    /// Run one accepted call and return its canonical value.
    ///
    /// # Errors
    ///
    /// Returns the body's failure (a coded harness error or a plain
    /// message), which the pipeline materializes as the error result.
    fn execute(
        &self,
        args: &Value,
        exec: &ToolExecution,
        signal: Arc<ToolSignal>,
        run: &mut ToolRunContext<'_>,
    ) -> Result<Value, ToolError>;

    /// Requests deferred loading of the tool definition.
    fn defer_loading(&self) -> bool {
        false
    }

    /// Cooperative timeout budget in milliseconds; never sent to the model.
    fn timeout_ms(&self) -> Option<f64> {
        None
    }

    /// Pure synchronous classifier for sibling overlap; only an exact `true`
    /// opts in. The default (undeclared) is exclusive, matching dsh's
    /// absent-method path.
    fn concurrency_safe(&self, _args: &Value) -> Option<bool> {
        None
    }

    /// Install execution-prepared content before post-execute policies.
    fn project_content(
        &self,
        _exec: &ToolExecution,
        _result: &ToolExecutionResult,
    ) -> Option<Vec<ContentBlock>> {
        None
    }

    /// Synchronous last-mile content transform for every normalized outcome;
    /// must be total.
    fn finalize_content(
        &self,
        _exec: &ToolExecution,
        _result: &ToolExecutionResult,
    ) -> Option<Vec<ContentBlock>> {
        None
    }

    /// How to present the pending state of one call, derived from the args.
    fn present_call(&self, _args: &Value) -> Option<ToolCallView> {
        None
    }

    /// How to present the completed state of one call.
    fn present_result(&self, _args: &Value, _result: &ToolResult) -> Option<ToolResultView> {
        None
    }
}

/// The builder counterpart of dsh `defineTool`.
pub struct DefineTool {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Author-facing parameter spec (implicit open object root).
    pub parameters: Value,
    /// Author-facing output value spec.
    pub output_schema: Value,
    /// Pure render projection.
    pub render: RenderProjector,
    /// Pure presentation projection, when any.
    pub presentation_meta: Option<PresentationProjector>,
    /// The tool body.
    pub execute: ToolBody,
    /// Deferred-loading request.
    pub defer_loading: bool,
    /// Cooperative timeout budget.
    pub timeout_ms: Option<f64>,
    /// Concurrency classifier; `None` leaves the call exclusive.
    pub concurrency_safe: Option<ConcurrencyClassifier>,
    /// Execution-prepared content projection.
    pub project_content: Option<ContentProjector>,
    /// Last-mile content transform.
    pub finalize_content: Option<ContentProjector>,
    /// Pending-state presenter.
    pub present_call: Option<PresentCallFn>,
    /// Completed-state presenter.
    pub present_result: Option<PresentResultFn>,
}

impl std::fmt::Debug for DefineTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The closures render as their static names (the hand-rolled Debug
        // judgment of ToolOutput).
        f.debug_struct("DefineTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .field("output_schema", &self.output_schema)
            .field("render", &type_name_of(&self.render))
            .field(
                "presentation_meta",
                &self.presentation_meta.as_ref().map(|m| type_name_of(m)),
            )
            .field("execute", &type_name_of(&self.execute))
            .field("defer_loading", &self.defer_loading)
            .field("timeout_ms", &self.timeout_ms)
            .field(
                "concurrency_safe",
                &self.concurrency_safe.as_ref().map(|c| type_name_of(c)),
            )
            .field(
                "project_content",
                &self.project_content.as_ref().map(|c| type_name_of(c)),
            )
            .field(
                "finalize_content",
                &self.finalize_content.as_ref().map(|c| type_name_of(c)),
            )
            .field(
                "present_call",
                &self.present_call.as_ref().map(|c| type_name_of(c)),
            )
            .field(
                "present_result",
                &self.present_result.as_ref().map(|c| type_name_of(c)),
            )
            .finish()
    }
}

impl DefineTool {
    /// Validates the specs and timeout, producing a built tool.
    ///
    /// # Errors
    ///
    /// Returns the first spec-compilation failure, or the dsh timeout
    /// validation error.
    pub fn build(self) -> Result<BuiltTool, DefineToolError> {
        if let Some(timeout_ms) = self.timeout_ms
            && (!timeout_ms.is_finite() || timeout_ms <= 0.0)
        {
            return Err(DefineToolError::TimeoutMs(format!(
                "defineTool({}): timeoutMs must be a positive finite number",
                self.name
            )));
        }
        let parameters = parameter_schema_spec_to_json_schema(&self.parameters)
            .map_err(DefineToolError::Parameters)?;
        let output_schema_value = value_schema_spec_to_json_schema(&self.output_schema)
            .map_err(DefineToolError::Output)?;
        assert_supported_json_schema(&output_schema_value).map_err(DefineToolError::Output)?;
        let render = Arc::clone(&self.render);
        let presentation_meta = self.presentation_meta.clone();
        Ok(BuiltTool {
            definition: self,
            parameters,
            output: ToolOutput {
                schema: output_schema_value,
                render,
                presentation_meta,
            },
        })
    }
}

/// A built first-party tool with compiled schemas.
#[derive(Debug)]
pub struct BuiltTool {
    definition: DefineTool,
    parameters: Value,
    output: ToolOutput,
}

impl ToolDefinition for BuiltTool {
    fn name(&self) -> &str {
        &self.definition.name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn parameters(&self) -> &Value {
        &self.parameters
    }

    fn output(&self) -> &ToolOutput {
        &self.output
    }

    fn execute(
        &self,
        args: &Value,
        _exec: &ToolExecution,
        _signal: Arc<ToolSignal>,
        run: &mut ToolRunContext<'_>,
    ) -> Result<Value, ToolError> {
        let violations = validate_json_schema_value(&self.parameters, args, "");
        if !violations.is_empty() {
            return Err(ToolError::new(
                crate::schema::INVALID_ARGS_CODE,
                "ToolArgsError",
                &format!("invalid arguments: {}", violations.join("; ")),
            ));
        }
        (self.definition.execute)(args, run)
    }

    fn defer_loading(&self) -> bool {
        self.definition.defer_loading
    }

    fn timeout_ms(&self) -> Option<f64> {
        self.definition.timeout_ms
    }

    fn concurrency_safe(&self, args: &Value) -> Option<bool> {
        let classifier = self.definition.concurrency_safe.as_ref()?;
        // A presentation-grade soft wrapper: invalid arguments classify
        // exclusive instead of failing (dsh soft-wraps the classifier).
        let violations = validate_json_schema_value(&self.parameters, args, "");
        if !violations.is_empty() {
            return Some(false);
        }
        Some(classifier(args))
    }

    fn project_content(
        &self,
        exec: &ToolExecution,
        result: &ToolExecutionResult,
    ) -> Option<Vec<ContentBlock>> {
        (self.definition.project_content.as_ref()?)(exec, result)
    }

    fn finalize_content(
        &self,
        exec: &ToolExecution,
        result: &ToolExecutionResult,
    ) -> Option<Vec<ContentBlock>> {
        (self.definition.finalize_content.as_ref()?)(exec, result)
    }

    fn present_call(&self, args: &Value) -> Option<ToolCallView> {
        let violations = validate_json_schema_value(&self.parameters, args, "");
        if !violations.is_empty() {
            return None;
        }
        (self.definition.present_call.as_ref()?)(args)
    }

    fn present_result(&self, args: &Value, result: &ToolResult) -> Option<ToolResultView> {
        let violations = validate_json_schema_value(&self.parameters, args, "");
        if !violations.is_empty() {
            return None;
        }
        (self.definition.present_result.as_ref()?)(args, result)
    }
}
