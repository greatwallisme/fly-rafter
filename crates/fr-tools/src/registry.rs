// Rust guideline compliant 2026-10-28
//! Tool registry and execution pipeline (dsh `core/tools/src/index.ts`
//! `ToolRuntime`, verbatim semantics on a synchronous per-call stage model).
//!
//! Where dsh drives the five stages over async waterfalls, this port runs
//! each call's pipeline sequentially — the async surface of dsh exists for
//! sibling concurrency, which the loop-side scheduler owns by running one
//! pipeline per call. Stage listeners are ordered callbacks (decision
//! Q101′A): registration order = trigger order, a listener that ignores its
//! continuation vetoes the rest (the dsh waterfall `Next` contract), and
//! around-dispatch listeners distinguish pass-through canonical results from
//! authored ones (the dsh `WeakMap` identity mark). The scoped layer system
//! is structurally absent in M1 (single global layer, judgment Q82-A); the
//! layer-shaped tables keep a Phase 18 extension path. Mutex poisoning
//! recovers by re-adopting the guard: pipeline closures run outside the
//! locks and their panics are contained by `catch_unwind`.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

use fr_wire::{ContentBlock, Message};
use indexmap::IndexMap;
use serde_json::Value;

use crate::definition::{ToolDefinition, ToolOutput, ToolRunContext};
use crate::json_schema::validate_json_schema_value;
use crate::signal::ToolSignal;
use crate::types::{
    PostToolDecision, PreToolDecision, RUN_CODE_NAME, ToolError, ToolExecution,
    ToolExecutionFailure, ToolExecutionInput, ToolExecutionMode, ToolExecutionResult,
    ToolExecutionSuccess, ToolExecutionToken, ToolFailure, ToolGuard, ToolPresentationMode,
    ToolSchema, invalid_output_error, text_content, tool_aborted_before_dispatch_result,
    tool_aborted_result, unknown_tool_error,
};

/// One failed registration (dsh `register` throws).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// The output schema fell outside the enforced subset.
    OutputSchema(String),
    /// The timeout budget failed the positive-finite check.
    TimeoutMs(String),
    /// The reserved PTC transport name.
    Reserved(String),
    /// A duplicate name in the global layer.
    Duplicate(String),
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutputSchema(text)
            | Self::TimeoutMs(text)
            | Self::Reserved(text)
            | Self::Duplicate(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for RegisterError {}

/// One failed restriction (dsh `restrict` throws; M1 has no scoped context,
/// so the scoped-context failure is the reachable arm).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictError(pub String);

impl std::fmt::Display for RestrictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RestrictError {}

/// The outcome an approval seam resolves to (dsh `ApprovalOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// The one grant value.
    AllowedOnce,
    /// The user rejected.
    Rejected,
    /// The approval channel was cancelled.
    Cancelled,
    /// No approval channel is available.
    Unavailable,
}

/// The approval seam consumed opportunistically by the ask path (dsh
/// `ctx.get('approval')`).
pub trait ApprovalSeam: Send + Sync {
    /// Resolves one approval request for a call; blocking is allowed (the
    /// pipeline runs on the call's worker thread).
    fn request(
        &self,
        exec: &ToolExecution,
        reason: Option<&str>,
        display_reason: Option<&str>,
    ) -> ApprovalOutcome;
}

/// Registry notification surface (`tools/change` and `tools/result` emits).
pub trait ToolsEmitter: Send + Sync {
    /// The available tool set changed.
    fn tools_change(&self);
    /// One final result.
    fn tools_result(&self, exec: &ToolExecution, result: &ToolExecutionResult);
}

/// One `tools/pre-execute` listener: receives the pending call and the
/// continuation returning the remaining chain's decision.
pub type PreExecuteListener = Arc<
    dyn Fn(&ToolExecution, &mut dyn FnMut() -> PreToolDecision) -> PreToolDecision + Send + Sync,
>;

/// One `tools/execute` listener (around-dispatch wrapper).
///
/// Receives the dispatch view and a continuation that takes the view onward
/// (wrappers hand the view to `next` while delegating, so the mutable signal
/// slot is never aliased); returns either the canonical result it received
/// or an authored one.
pub type ExecuteListener = Arc<
    dyn Fn(
            &mut ToolDispatchView<'_>,
            &mut dyn FnMut(&mut ToolDispatchView<'_>) -> ToolExecutionResult,
        ) -> ToolExecuteOutcome
        + Send
        + Sync,
>;

/// One `tools/post-execute` listener: may accept, replace one projection, or
/// block.
pub type PostExecuteListener = Arc<
    dyn Fn(
            &ToolExecution,
            &ToolExecutionResult,
            &mut dyn FnMut() -> PostToolDecision,
        ) -> PostToolDecision
        + Send
        + Sync,
>;

/// The three decision stages accepting listener registration (Q101′A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListenerStage {
    /// The pre-dispatch allow/deny/cancel/ask gate.
    PreExecute,
    /// The around-dispatch wrapper.
    Execute,
    /// The post-dispatch accept/replace/block gate.
    PostExecute,
}

/// The result flavor an around-dispatch listener returns.
#[derive(Debug, Clone)]
pub enum ToolExecuteOutcome {
    /// The canonical result received from the continuation, unchanged (the
    /// dsh object-identity passthrough).
    Canonical(ToolExecutionResult),
    /// An authored or modified result, normalized through the output
    /// contract (the dsh non-canonical path).
    Authored(ToolExecutionResult),
}

/// Around-dispatch view of one execution: the immutable call identity plus
/// the replaceable dispatch signal (dsh `ToolDispatchExecution`).
#[derive(Debug)]
pub struct ToolDispatchView<'a> {
    /// The allowed call about to dispatch.
    pub exec: &'a ToolExecution,
    /// The cancellation signal visible to the next wrapper or the body;
    /// wrappers restore the upstream signal before returning.
    pub signal: Arc<ToolSignal>,
}

/// The stage a preparation hands the scheduler (dsh
/// `ScheduledToolPreparation`).
#[derive(Debug)]
pub enum ScheduledPreparation {
    /// Run the around-dispatch and body stages.
    Dispatch(ToolExecution),
    /// Skip dispatch; the result still receives post-execute.
    PostResult(ToolExecution, ToolExecutionResult),
    /// Skip post-execute; the result already matches failure semantics.
    FinalResult(ToolExecution, ToolExecutionResult),
}

/// The stage a dispatch hands the finalizer (dsh `ScheduledToolDispatch`).
#[derive(Debug)]
pub enum ScheduledDispatch {
    /// The result still receives post-execute.
    PostResult(ToolExecutionResult),
    /// The result already matches failure semantics.
    FinalResult(ToolExecutionResult),
}

/// Registry-owned per-call state (the dsh `WeakMap` tables keyed by the
/// execution object; keyed here by its token).
struct CallState {
    caller_signal: Arc<ToolSignal>,
    body_invoked: bool,
    deferred: Vec<Message>,
    concluding: bool,
    finalizer: Option<Arc<dyn ToolDefinition>>,
    projector: Option<Arc<dyn ToolDefinition>>,
}

/// The mutable registry face.
struct RegistryState {
    /// The single global layer (the Phase 18 scoped extension adds layer
    /// maps around this table).
    tools: IndexMap<String, Arc<dyn ToolDefinition>>,
    guards: Vec<ToolGuard>,
    executions: HashMap<u64, CallState>,
    next_token: u64,
    /// The reserved PTC transport, installed by the Phase 17 runtime.
    ptc_transport: Option<Arc<dyn ToolDefinition>>,
}

/// Tool registry and execution pipeline.
pub struct ToolRegistry {
    state: Mutex<RegistryState>,
    pre_execute: Mutex<Vec<PreExecuteListener>>,
    execute: Mutex<Vec<ExecuteListener>>,
    post_execute: Mutex<Vec<PostExecuteListener>>,
    default_mode: ToolPresentationMode,
    approval: Mutex<Option<Arc<dyn ApprovalSeam>>>,
    emitter: Arc<dyn ToolsEmitter>,
}

/// Recovers a registry lock from poisoning (closures run outside locks and
/// their panics are contained, so the guard re-adopts).
fn recover<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A registration disposer: dropping it unregisters the tool.
#[derive(Debug)]
pub struct ToolRegistration {
    registry: Arc<ToolRegistry>,
    name: String,
}

impl Drop for ToolRegistration {
    fn drop(&mut self) {
        let changed = {
            let mut state = recover(&self.registry.state);
            state.tools.shift_remove(&self.name).is_some()
        };
        if changed {
            self.registry.emitter.tools_change();
        }
    }
}

/// A guard registration disposer.
#[derive(Debug)]
pub struct GuardRegistration {
    registry: Arc<ToolRegistry>,
    position: usize,
}

impl Drop for GuardRegistration {
    fn drop(&mut self) {
        let mut state = recover(&self.registry.state);
        if self.position < state.guards.len() {
            drop(state.guards.remove(self.position));
        }
    }
}

/// A listener registration disposer (per stage, registration order).
#[derive(Debug)]
pub struct ListenerRegistration {
    registry: Arc<ToolRegistry>,
    stage: ListenerStage,
    position: usize,
}

impl Drop for ListenerRegistration {
    fn drop(&mut self) {
        match self.stage {
            ListenerStage::PreExecute => {
                let mut list = recover(&self.registry.pre_execute);
                if self.position < list.len() {
                    list.remove(self.position);
                }
            }
            ListenerStage::Execute => {
                let mut list = recover(&self.registry.execute);
                if self.position < list.len() {
                    list.remove(self.position);
                }
            }
            ListenerStage::PostExecute => {
                let mut list = recover(&self.registry.post_execute);
                if self.position < list.len() {
                    list.remove(self.position);
                }
            }
        }
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The listener tables hold closures; the summary carries the mode
        // and table sizes.
        let tool_count = recover(&self.state).tools.len();
        let guard_count = recover(&self.state).guards.len();
        f.debug_struct("ToolRegistry")
            .field("default_mode", &self.default_mode)
            .field("tool_count", &tool_count)
            .field("guard_count", &guard_count)
            .field("pre_execute_count", &recover(&self.pre_execute).len())
            .field("execute_count", &recover(&self.execute).len())
            .field("post_execute_count", &recover(&self.post_execute).len())
            .finish_non_exhaustive()
    }
}

impl ToolRegistry {
    /// Builds a registry with the deployment presentation mode and its
    /// notification surface.
    #[must_use]
    pub fn new(default_mode: ToolPresentationMode, emitter: Arc<dyn ToolsEmitter>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(RegistryState {
                tools: IndexMap::new(),
                guards: Vec::new(),
                executions: HashMap::new(),
                next_token: 0,
                ptc_transport: None,
            }),
            pre_execute: Mutex::new(Vec::new()),
            execute: Mutex::new(Vec::new()),
            post_execute: Mutex::new(Vec::new()),
            default_mode,
            approval: Mutex::new(None),
            emitter,
        })
    }

    /// Installs the approval seam; an absent seam keeps the ask degrade (dsh
    /// `ctx.get('approval')` opportunism).
    pub fn set_approval(&self, approval: Option<Arc<dyn ApprovalSeam>>) {
        *recover(&self.approval) = approval;
    }

    /// Installs the reserved PTC transport (the Phase 17 runtime hook).
    pub fn set_ptc_transport(&self, transport: Option<Arc<dyn ToolDefinition>>) {
        recover(&self.state).ptc_transport = transport;
    }

    /// The deployment presentation mode.
    #[must_use]
    pub const fn default_mode(&self) -> ToolPresentationMode {
        self.default_mode
    }

    /// Registers a tool in the global layer.
    ///
    /// # Errors
    ///
    /// Returns the dsh registration failures: output-schema subset
    /// violations, the timeout validation, the reserved transport name, and
    /// duplicates within the layer.
    pub fn register(
        self: &Arc<Self>,
        definition: Arc<dyn ToolDefinition>,
    ) -> Result<ToolRegistration, RegisterError> {
        let name = definition.name().to_owned();
        let output: &ToolOutput = definition.output();
        crate::json_schema::assert_supported_json_schema(&output.schema)
            .map_err(|error| RegisterError::OutputSchema(error.to_string()))?;
        if let Some(timeout_ms) = definition.timeout_ms()
            && (!timeout_ms.is_finite() || timeout_ms <= 0.0)
        {
            return Err(RegisterError::TimeoutMs(format!(
                "tool \"{name}\" timeoutMs must be a positive finite number"
            )));
        }
        if name == RUN_CODE_NAME {
            return Err(RegisterError::Reserved(format!(
                "tool name \"{RUN_CODE_NAME}\" is reserved for the PTC mode presentation transport and cannot be registered or shadowed"
            )));
        }
        let mut state = recover(&self.state);
        if state.tools.contains_key(&name) {
            return Err(RegisterError::Duplicate(format!(
                "tool \"{name}\" is already registered (for a per-agent variant, register through that agent's `agent.ctx` instead)"
            )));
        }
        state.tools.insert(name.clone(), definition);
        drop(state);
        self.emitter.tools_change();
        Ok(ToolRegistration {
            registry: Arc::clone(self),
            name,
        })
    }

    /// Restricts global tools for a calling agent scope; M1 has no scoped
    /// context, so the dsh scoped-context failure is the reachable arm.
    ///
    /// # Errors
    ///
    /// Returns the dsh scoped-context failure verbatim.
    pub fn restrict(&self) -> Result<(), RestrictError> {
        Err(RestrictError(
            "tools.restrict() requires a scoped context (agent.ctx): a context-global restriction would mask every agent — deny the tool for the intended agent instead".to_owned(),
        ))
    }

    /// Registers a monotonic guard (no `tools/change` notification).
    #[must_use]
    pub fn guard(self: &Arc<Self>, guard: ToolGuard) -> GuardRegistration {
        let position = {
            let mut state = recover(&self.state);
            state.guards.push(guard);
            state.guards.len() - 1
        };
        GuardRegistration {
            registry: Arc::clone(self),
            position,
        }
    }

    /// Registers one `tools/pre-execute` listener.
    #[must_use]
    pub fn on_pre_execute(self: &Arc<Self>, listener: PreExecuteListener) -> ListenerRegistration {
        let position = {
            let mut list = recover(&self.pre_execute);
            list.push(listener);
            list.len() - 1
        };
        ListenerRegistration {
            registry: Arc::clone(self),
            stage: ListenerStage::PreExecute,
            position,
        }
    }

    /// Registers one `tools/execute` listener.
    #[must_use]
    pub fn on_execute(self: &Arc<Self>, listener: ExecuteListener) -> ListenerRegistration {
        let position = {
            let mut list = recover(&self.execute);
            list.push(listener);
            list.len() - 1
        };
        ListenerRegistration {
            registry: Arc::clone(self),
            stage: ListenerStage::Execute,
            position,
        }
    }

    /// Registers one `tools/post-execute` listener.
    #[must_use]
    pub fn on_post_execute(
        self: &Arc<Self>,
        listener: PostExecuteListener,
    ) -> ListenerRegistration {
        let position = {
            let mut list = recover(&self.post_execute);
            list.push(listener);
            list.len() - 1
        };
        ListenerRegistration {
            registry: Arc::clone(self),
            stage: ListenerStage::PostExecute,
            position,
        }
    }

    /// Looks up a visible tool (M1: the global layer plus the transport
    /// insertion under a non-native mode).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn ToolDefinition>> {
        let state = recover(&self.state);
        if let Some(definition) = state.tools.get(name) {
            return Some(Arc::clone(definition));
        }
        if self.default_mode != ToolPresentationMode::Native && name == RUN_CODE_NAME {
            return state.ptc_transport.clone();
        }
        None
    }

    /// Whether the `ptc` mode collapse denies a model-direct call (nested
    /// sub-dispatches bypass the collapse).
    #[must_use]
    pub fn collapses(&self, name: &str, nested: bool) -> bool {
        !nested && self.default_mode == ToolPresentationMode::Ptc && name != RUN_CODE_NAME
    }

    /// Resolves the definition that may execute for a call (dsh
    /// `resolveExecution`).
    fn resolve_execution(
        &self,
        name: &str,
        nested: bool,
    ) -> Result<Arc<dyn ToolDefinition>, ToolError> {
        if self.get(name).is_some() && self.collapses(name, nested) {
            return Err(unknown_tool_error(name, Some(&collapse_route(name))));
        }
        self.get(name).ok_or_else(|| unknown_tool_error(name, None))
    }

    /// Classifies a pending call; only an exact `true` is parallel, and a
    /// panicking classifier is exclusive (dsh try/catch fail-closed).
    #[must_use]
    pub fn execution_mode(&self, name: &str, arguments: &Value, nested: bool) -> ToolExecutionMode {
        let Ok(tool) = self.resolve_execution(name, nested) else {
            return ToolExecutionMode::Exclusive;
        };
        match catch_unwind(AssertUnwindSafe(|| tool.concurrency_safe(arguments))) {
            Ok(Some(true)) => ToolExecutionMode::Parallel,
            Ok(_) | Err(_) => ToolExecutionMode::Exclusive,
        }
    }

    /// The visible definitions in registration order (M1: the global layer,
    /// plus the transport last under a non-native mode).
    #[must_use]
    pub fn visible_tools(&self) -> Vec<Arc<dyn ToolDefinition>> {
        let mut tools: Vec<Arc<dyn ToolDefinition>> =
            recover(&self.state).tools.values().cloned().collect();
        if self.default_mode != ToolPresentationMode::Native
            && let Some(transport) = recover(&self.state).ptc_transport.clone()
        {
            tools.push(transport);
        }
        tools
    }

    /// Projects visible definitions onto the model-facing schema fields.
    #[must_use]
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.visible_tools()
            .into_iter()
            .map(|definition| ToolSchema {
                name: definition.name().to_owned(),
                description: definition.description().to_owned(),
                parameters: definition.parameters().clone(),
                defer_loading: definition.defer_loading().then_some(true),
            })
            .collect()
    }

    /// Builds the wire schemas and known names for prompt assembly; a mode
    /// collapse keeps only the transport.
    ///
    /// # Errors
    ///
    /// Returns the dsh PTC-runtime misconfiguration error under a non-native
    /// mode without an installed transport.
    pub fn wire_schemas(&self) -> Result<(Vec<ToolSchema>, Vec<String>), String> {
        if self.default_mode == ToolPresentationMode::Native {
            let schemas = self.schemas();
            let known_names = self
                .visible_tools()
                .iter()
                .map(|definition| definition.name().to_owned())
                .collect();
            return Ok((schemas, known_names));
        }
        let transport = recover(&self.state).ptc_transport.clone();
        let Some(transport) = transport else {
            return Err(
                "dsh-tools: mode \"ptc\" requires a PTC runtime — load a ctx.ptcRuntime implementation (e.g. @deepseek-ai/dsh-ptc-runtime-node) or set tools mode to \"native\""
                    .to_owned(),
            );
        };
        let schema = ToolSchema {
            name: transport.name().to_owned(),
            description: transport.description().to_owned(),
            parameters: transport.parameters().clone(),
            defer_loading: transport.defer_loading().then_some(true),
        };
        Ok((vec![schema], vec![RUN_CODE_NAME.to_owned()]))
    }

    /// First monotonic denial from the registered guards, in registration
    /// order (a panicking guard reads as no denial from this slot; the dsh
    /// contract has no such case because JS guards do not throw without
    /// failing the call).
    fn guard_reason(&self, exec: &ToolExecution) -> Option<String> {
        let guards: Vec<ToolGuard> = recover(&self.state).guards.clone();
        for guard in &guards {
            if let Ok(Some(reason)) = catch_unwind(AssertUnwindSafe(|| guard(exec))) {
                return Some(reason);
            }
        }
        None
    }

    /// Executes a call through the complete pipeline (the dsh `execute`
    /// composite; every stage runs on the calling thread).
    pub fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        match self.prepare_scheduled(input) {
            ScheduledPreparation::Dispatch(exec) => match self.dispatch_scheduled(&exec) {
                Ok(ScheduledDispatch::PostResult(result)) => self.finalize_scheduled(&exec, result),
                Ok(ScheduledDispatch::FinalResult(result)) => self.finish(&exec, result),
                Err(result) => self.finish(&exec, *result),
            },
            ScheduledPreparation::PostResult(exec, result) => {
                self.finalize_scheduled(&exec, result)
            }
            ScheduledPreparation::FinalResult(exec, result) => self.finish(&exec, result),
        }
    }

    /// Aborts the caller-owned signal of one in-flight call (the
    /// service-face cancel path); returns whether the token was live.
    pub fn abort_caller(&self, token: ToolExecutionToken) -> bool {
        let signal = recover(&self.state)
            .executions
            .get(&token)
            .map(|call| Arc::clone(&call.caller_signal));
        signal.is_some_and(|signal| {
            signal.abort_caller();
            true
        })
    }

    /// Stage 1: materialize the execution and run the ordered pre-execute
    /// and guard gate (dsh `prepareScheduledExecution`).
    pub fn prepare_scheduled(&self, input: &ToolExecutionInput) -> ScheduledPreparation {
        let (exec, collapsed_final) = self.create_execution(input);
        if let Some(result) = collapsed_final {
            return ScheduledPreparation::FinalResult(exec, result);
        }
        if self.caller_aborted(&exec) {
            return ScheduledPreparation::FinalResult(
                exec,
                tool_aborted_before_dispatch_result(None),
            );
        }
        let gate = self.waterfall_pre_execute(&exec);
        let (decision, approval_cancelled) = match gate {
            PreToolDecision::Ask {
                reason,
                display_reason,
            } => self.service_ask(&exec, reason.as_deref(), display_reason.as_deref()),
            other => (other, false),
        };
        if self.caller_aborted(&exec) && approval_cancelled {
            return ScheduledPreparation::PostResult(
                exec,
                tool_aborted_before_dispatch_result(None),
            );
        }
        if matches!(decision, PreToolDecision::Cancel) {
            return ScheduledPreparation::PostResult(
                exec,
                tool_aborted_before_dispatch_result(None),
            );
        }
        let (denial_reason, denial_info) = match &decision {
            PreToolDecision::Allow => (self.guard_reason(&exec), None),
            PreToolDecision::Deny { reason, info } => (Some(reason.clone()), info.clone()),
            PreToolDecision::Cancel | PreToolDecision::Ask { .. } => (None, None),
        };
        if let Some(reason) = denial_reason {
            let failure = ToolFailure {
                info: denial_info,
                message: reason,
            };
            let message = failure.message.clone();
            return ScheduledPreparation::PostResult(
                exec,
                ToolExecutionResult::Failure(ToolExecutionFailure {
                    content: text_content(&format!("Error: {message}")),
                    error: failure,
                    meta: None,
                    additional_contexts: Vec::new(),
                }),
            );
        }
        if self.caller_aborted(&exec) {
            return ScheduledPreparation::PostResult(
                exec,
                tool_aborted_before_dispatch_result(None),
            );
        }
        ScheduledPreparation::Dispatch(exec)
    }

    /// Stage 2: run the around-dispatch waterfall and the tool body (dsh
    /// `dispatchScheduledExecution`); pipeline errors become final results.
    ///
    /// # Errors
    ///
    /// Returns the materialized failure when a pipeline stage raises (the
    /// dsh outer catch); the caller routes it to the finish stage.
    pub fn dispatch_scheduled(
        &self,
        exec: &ToolExecution,
    ) -> Result<ScheduledDispatch, Box<ToolExecutionResult>> {
        let listeners = recover(&self.execute).clone();
        let mut view = ToolDispatchView {
            exec,
            signal: Arc::clone(&exec.signal),
        };
        let outcome = self.run_execute_chain(&listeners, &mut view);
        let result = match outcome {
            ToolExecuteOutcome::Canonical(result) => result,
            ToolExecuteOutcome::Authored(result) => self.normalize_dispatch_result(exec, result)?,
        };
        let deferred = self.take_deferred(exec);
        let result = merge_deferred_contexts(result, deferred);
        if self.caller_aborted(exec) && !result.is_error() {
            return Ok(ScheduledDispatch::PostResult(
                self.cancellation_result(exec, Some(&result)),
            ));
        }
        Ok(ScheduledDispatch::PostResult(result))
    }

    /// Runs the execute chain over the remaining listeners; the tail
    /// dispatches the fused body.
    fn run_execute_chain(
        &self,
        listeners: &[ExecuteListener],
        view: &mut ToolDispatchView<'_>,
    ) -> ToolExecuteOutcome {
        match listeners.split_first() {
            None => ToolExecuteOutcome::Canonical(self.dispatch_tool_body(view)),
            Some((listener, rest)) => {
                // A nested authored outcome surfaces to its wrapper as an
                // ordinary result value; only the OUTERMOST outcome is
                // normalized (dsh normalizes once, at the registry
                // boundary), so both arms hand the value through.
                listener(view, &mut |view| {
                    let outcome = self.run_execute_chain(rest, view);
                    match outcome {
                        ToolExecuteOutcome::Canonical(result)
                        | ToolExecuteOutcome::Authored(result) => result,
                    }
                })
            }
        }
    }

    /// Stage 3: run post-execute, then finalization (dsh
    /// `finalizeScheduledExecution`); errors become finished error results.
    pub fn finalize_scheduled(
        &self,
        exec: &ToolExecution,
        result: ToolExecutionResult,
    ) -> ToolExecutionResult {
        let projected = self.project_content(exec, result);
        let post_result = match self.post_execute(exec, projected) {
            Ok(result) => result,
            Err(error_result) => return self.finish(exec, *error_result),
        };
        let final_result = if self.caller_aborted(exec) && !post_result.is_error() {
            self.cancellation_result(exec, Some(&post_result))
        } else {
            post_result
        };
        self.finish(exec, final_result)
    }

    /// Stage 4: apply definition-owned finalization and notify (dsh
    /// `finishScheduledExecution`); the materialization `TypeError` paths
    /// are structurally absent for owned values.
    pub fn finish(&self, exec: &ToolExecution, result: ToolExecutionResult) -> ToolExecutionResult {
        let finalizer = recover(&self.state)
            .executions
            .remove(&exec.token)
            .and_then(|call| call.finalizer);
        let final_result = match finalizer {
            Some(definition) => {
                match catch_unwind(AssertUnwindSafe(|| {
                    definition.finalize_content(exec, &result)
                })) {
                    Ok(Some(content)) => replace_content(result, content),
                    Ok(None) | Err(_) => result,
                }
            }
            None => result,
        };
        self.emitter.tools_result(exec, &final_result);
        final_result
    }

    /// Creates one registry-minted execution (dsh `createExecution`); the
    /// arguments snapshot `TypeError` is structurally absent (owned values
    /// are lossless JSON by construction).
    fn create_execution(
        &self,
        input: &ToolExecutionInput,
    ) -> (ToolExecution, Option<ToolExecutionResult>) {
        let visible = self.get(&input.name);
        let collapsed = visible.is_some() && self.collapses(&input.name, input.parent.is_some());
        let mut state = recover(&self.state);
        state.next_token += 1;
        let token = state.next_token;
        let finalizer_for = if collapsed && !input.signal.is_aborted() {
            None
        } else {
            visible.clone()
        };
        let projector = if collapsed { None } else { visible.clone() };
        state.executions.insert(
            token,
            CallState {
                caller_signal: Arc::clone(&input.signal),
                body_invoked: false,
                deferred: Vec::new(),
                concluding: false,
                finalizer: finalizer_for,
                projector,
            },
        );
        drop(state);
        let exec = ToolExecution {
            token,
            root_call_id: input
                .root_call_id
                .clone()
                .unwrap_or_else(|| input.call_id.clone()),
            call_id: input.call_id.clone(),
            name: input.name.clone(),
            schema: input.schema.clone(),
            arguments: input.arguments.clone(),
            agent: input.agent.clone(),
            parent: input.parent,
            signal: Arc::clone(&input.signal),
        };
        if collapsed {
            if input.signal.is_aborted() {
                return (exec, Some(tool_aborted_before_dispatch_result(None)));
            }
            return (
                exec,
                Some(tool_error_result(&unknown_tool_error(
                    &input.name,
                    Some(&collapse_route(&input.name)),
                ))),
            );
        }
        (exec, None)
    }

    /// Whether the original caller signal is currently aborted.
    fn caller_aborted(&self, exec: &ToolExecution) -> bool {
        recover(&self.state)
            .executions
            .get(&exec.token)
            .is_some_and(|call| call.caller_signal.is_aborted())
    }

    /// The canonical cancellation outcome selected by whether the body
    /// started.
    fn cancellation_result(
        &self,
        exec: &ToolExecution,
        prior: Option<&ToolExecutionResult>,
    ) -> ToolExecutionResult {
        let body_invoked = recover(&self.state)
            .executions
            .get(&exec.token)
            .is_some_and(|call| call.body_invoked);
        if body_invoked {
            tool_aborted_result(prior)
        } else {
            tool_aborted_before_dispatch_result(prior)
        }
    }

    /// Takes the contexts deferred by the body.
    fn take_deferred(&self, exec: &ToolExecution) -> Vec<Message> {
        recover(&self.state)
            .executions
            .get_mut(&exec.token)
            .map_or_else(Vec::new, |call| std::mem::take(&mut call.deferred))
    }

    /// Runs the `tools/pre-execute` waterfall (default allow).
    fn waterfall_pre_execute(&self, exec: &ToolExecution) -> PreToolDecision {
        let listeners = recover(&self.pre_execute).clone();
        run_pre_execute_chain(&listeners, exec)
    }

    /// Resolves an `ask` decision through the approval seam (dsh
    /// `serviceAsk`).
    fn service_ask(
        &self,
        exec: &ToolExecution,
        reason: Option<&str>,
        display_reason: Option<&str>,
    ) -> (PreToolDecision, bool) {
        let approval = recover(&self.approval).clone();
        let Some(approval) = approval else {
            return (
                PreToolDecision::Deny {
                    reason: reason.map_or_else(
                        || {
                            format!(
                                "tool \"{}\" requires approval (not yet supported)",
                                exec.name
                            )
                        },
                        str::to_owned,
                    ),
                    info: None,
                },
                false,
            );
        };
        if exec.agent.is_none() {
            return (
                PreToolDecision::Deny {
                    reason: format!(
                        "tool \"{}\" requires approval, but the call has no agent to route it through",
                        exec.name
                    ),
                    info: None,
                },
                false,
            );
        }
        match approval.request(exec, reason, display_reason) {
            ApprovalOutcome::AllowedOnce => (PreToolDecision::Allow, false),
            ApprovalOutcome::Rejected => (
                PreToolDecision::Deny {
                    reason: format!("the user rejected tool \"{}\"", exec.name),
                    info: None,
                },
                false,
            ),
            ApprovalOutcome::Cancelled => (
                PreToolDecision::Deny {
                    reason: format!("approval for tool \"{}\" was cancelled", exec.name),
                    info: None,
                },
                true,
            ),
            ApprovalOutcome::Unavailable => (
                PreToolDecision::Deny {
                    reason: format!(
                        "tool \"{}\" requires approval, but no approval channel is available",
                        exec.name
                    ),
                    info: None,
                },
                false,
            ),
        }
    }

    /// Dispatches the registered body with the caller signal fused into any
    /// around-wrapper replacement (dsh `dispatchToolBody`); the wrapper
    /// signal is restored before returning.
    fn dispatch_tool_body(&self, view: &mut ToolDispatchView<'_>) -> ToolExecutionResult {
        let caller = recover(&self.state)
            .executions
            .get(&view.exec.token)
            .map(|call| Arc::clone(&call.caller_signal));
        let Some(caller) = caller else {
            return tool_error_result(&ToolError::plain(
                "tool registry scheduler invariant violated: missing cancellation state",
            ));
        };
        let wrapper_signal = Arc::clone(&view.signal);
        let fused = ToolSignal::fuse(caller, Arc::clone(&view.signal));
        if fused.is_aborted() {
            view.signal = wrapper_signal;
            return tool_aborted_before_dispatch_result(None);
        }
        let tool = match self.resolve_execution(&view.exec.name, view.exec.nested()) {
            Ok(tool) => tool,
            Err(error) => {
                view.signal = wrapper_signal;
                return tool_error_result(&error);
            }
        };
        if let Some(call) = recover(&self.state).executions.get_mut(&view.exec.token) {
            call.body_invoked = true;
        }
        let mut run = ToolRunContext::new(view.exec, Arc::clone(&fused));
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            tool.execute(
                &view.exec.arguments,
                view.exec,
                Arc::clone(&fused),
                &mut run,
            )
        }));
        let (deferred, concluded) = (run.take_deferred_contexts(), run.concluded_turn());
        self.record_body_outcome(view.exec, deferred, concluded);
        let result = match outcome {
            Ok(Ok(value)) => match self.create_success_result(view.exec, &tool, value) {
                Ok(mut success) => {
                    success.concludes_turn |= concluded;
                    ToolExecutionResult::Success(success)
                }
                Err(error) => tool_error_result(&error),
            },
            Ok(Err(error)) => tool_error_result(&error),
            Err(panic) => tool_error_result(&ToolError::plain(&panic_message(&panic))),
        };
        view.signal = wrapper_signal;
        if fused.is_aborted() {
            return tool_aborted_result(Some(&result));
        }
        result
    }

    /// Stores the body's deferred contexts and turn conclusion.
    fn record_body_outcome(&self, exec: &ToolExecution, deferred: Vec<Message>, concluded: bool) {
        if let Some(call) = recover(&self.state).executions.get_mut(&exec.token) {
            call.deferred = deferred;
            call.concluding |= concluded;
        }
    }

    /// Validates, renders, and projects one successful body value (dsh
    /// `createSuccessResult`); projector panics surface through the
    /// invalid-output violation family.
    fn create_success_result(
        &self,
        exec: &ToolExecution,
        tool: &Arc<dyn ToolDefinition>,
        value: Value,
    ) -> Result<ToolExecutionSuccess, ToolError> {
        let output = tool.output();
        let violations = validate_json_schema_value(&output.schema, &value, "value");
        if !violations.is_empty() {
            return Err(invalid_output_error(tool.name(), &violations));
        }
        let content = match catch_unwind(AssertUnwindSafe(|| {
            (output.render)(&exec.arguments, &value)
        })) {
            Ok(content) => content,
            Err(panic) => {
                return Err(invalid_output_error(
                    tool.name(),
                    &[format!("output.render failed: {}", panic_message(&panic))],
                ));
            }
        };
        let meta = match &output.presentation_meta {
            None => None,
            Some(projector) => {
                match catch_unwind(AssertUnwindSafe(|| projector(&exec.arguments, &value))) {
                    Ok(meta) => Some(meta),
                    Err(panic) => {
                        return Err(invalid_output_error(
                            tool.name(),
                            &[format!(
                                "output.presentationMeta failed: {}",
                                panic_message(&panic)
                            )],
                        ));
                    }
                }
            }
        };
        let concludes_turn = recover(&self.state)
            .executions
            .get(&exec.token)
            .is_some_and(|call| call.concluding);
        Ok(ToolExecutionSuccess {
            value,
            content,
            meta,
            additional_contexts: Vec::new(),
            concludes_turn,
        })
    }

    /// Normalizes an authored around-dispatch result through the owning
    /// output contract (dsh `normalizeDispatchResult`).
    fn normalize_dispatch_result(
        &self,
        exec: &ToolExecution,
        result: ToolExecutionResult,
    ) -> Result<ToolExecutionResult, Box<ToolExecutionResult>> {
        match result {
            failure @ ToolExecutionResult::Failure(_) => Ok(failure),
            ToolExecutionResult::Success(success) => {
                let tool = self
                    .resolve_execution(&exec.name, exec.nested())
                    .map_err(|error| Box::new(tool_error_result(&error)))?;
                let mut normalized = self
                    .create_success_result(exec, &tool, success.value)
                    .map_err(|error| Box::new(tool_error_result(&error)))?;
                normalized.additional_contexts = success.additional_contexts;
                Ok(ToolExecutionResult::Success(normalized))
            }
        }
    }

    /// Applies the definition-owned content projection before post-execute
    /// (dsh `finalizeScheduledExecution` head); a panicking projector keeps
    /// the unprojected result.
    fn project_content(
        &self,
        exec: &ToolExecution,
        result: ToolExecutionResult,
    ) -> ToolExecutionResult {
        let projector = recover(&self.state)
            .executions
            .get_mut(&exec.token)
            .and_then(|call| call.projector.take());
        let Some(definition) = projector else {
            return result;
        };
        match catch_unwind(AssertUnwindSafe(|| {
            definition.project_content(exec, &result)
        })) {
            Ok(Some(content)) => replace_content(result, content),
            Ok(None) | Err(_) => result,
        }
    }

    /// Runs the `tools/post-execute` waterfall and applies its decision (dsh
    /// `postExecute`).
    fn post_execute(
        &self,
        exec: &ToolExecution,
        result: ToolExecutionResult,
    ) -> Result<ToolExecutionResult, Box<ToolExecutionResult>> {
        let listeners = recover(&self.post_execute).clone();
        let decision = run_post_execute_chain(&listeners, exec, &result);
        match decision {
            PostToolDecision::Block {
                feedback,
                additional_contexts,
            } => {
                let message = failure_message_from_content(&feedback);
                Ok(ToolExecutionResult::Failure(ToolExecutionFailure {
                    content: feedback,
                    error: ToolFailure {
                        message,
                        info: None,
                    },
                    meta: None,
                    additional_contexts,
                }))
            }
            PostToolDecision::Accept {
                content,
                value,
                additional_contexts,
            } => {
                if content.is_some() && value.is_some() {
                    return Err(Box::new(tool_error_result(&type_error(
                        "tools/post-execute accept decision cannot replace both value and content",
                    ))));
                }
                let mut merged = result.additional_contexts_to_owned();
                merged.extend(additional_contexts);
                if let Some(value) = value {
                    if result.is_error() {
                        return Err(Box::new(tool_error_result(&type_error(
                            "tools/post-execute cannot replace the value of a failed result",
                        ))));
                    }
                    let tool = self
                        .resolve_execution(&exec.name, exec.nested())
                        .map_err(|error| Box::new(tool_error_result(&error)))?;
                    let mut replaced = self
                        .create_success_result(exec, &tool, value)
                        .map_err(|error| Box::new(tool_error_result(&error)))?;
                    replaced.additional_contexts = merged;
                    return Ok(ToolExecutionResult::Success(replaced));
                }
                Ok(match content {
                    Some(content) => replace_content(result, content),
                    None => result,
                }
                .with_additional_contexts(merged))
            }
        }
    }
}

/// The collapse-route suffix of a mode-collapsed denial (dsh
/// `ToolNotFoundError` reachableFrom).
fn collapse_route(name: &str) -> String {
    format!(
        "only `{RUN_CODE_NAME}` is callable directly — call `{name}` from inside a `{RUN_CODE_NAME}` program instead"
    )
}

/// A `TypeError`-classed pipeline error.
fn type_error(message: &str) -> ToolError {
    ToolError::new("TYPE_ERROR", "TypeError", message)
}

/// Runs the pre-execute chain (default allow).
fn run_pre_execute_chain(
    listeners: &[PreExecuteListener],
    exec: &ToolExecution,
) -> PreToolDecision {
    match listeners.split_first() {
        None => PreToolDecision::Allow,
        Some((listener, rest)) => listener(exec, &mut || run_pre_execute_chain(rest, exec)),
    }
}

/// Runs the post-execute chain (default accept).
fn run_post_execute_chain(
    listeners: &[PostExecuteListener],
    exec: &ToolExecution,
    result: &ToolExecutionResult,
) -> PostToolDecision {
    match listeners.split_first() {
        None => PostToolDecision::Accept {
            content: None,
            value: None,
            additional_contexts: Vec::new(),
        },
        Some((listener, rest)) => listener(exec, result, &mut || {
            run_post_execute_chain(rest, exec, result)
        }),
    }
}

/// The canonical failure result of one raised error (dsh `toolErrorResult`).
fn tool_error_result(error: &ToolError) -> ToolExecutionResult {
    ToolExecutionResult::Failure(ToolExecutionFailure {
        content: text_content(&format!("Error: {}", error.failure.message)),
        error: error.failure.clone(),
        meta: None,
        additional_contexts: Vec::new(),
    })
}

/// Derives one failure message from policy feedback (dsh
/// `failureMessageFromContent`).
fn failure_message_from_content(content: &[ContentBlock]) -> String {
    let text = content
        .iter()
        .map(|block| match block {
            ContentBlock::Known(fr_wire::KnownContentBlock::Text { text }) => text.clone(),
            other => format!("[{} content]", block_type_of(other)),
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        "tool result blocked by post-execute policy".to_owned()
    } else {
        text
    }
}

/// The wire `type` tag of one content block (for the `[type content]`
/// fallback rendering).
fn block_type_of(block: &ContentBlock) -> String {
    serde_json::to_value(block)
        .ok()
        .and_then(|value| {
            value
                .as_object()
                .and_then(|object| object.get("type"))
                .cloned()
        })
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Replaces the content of either outcome, preserving every other field.
fn replace_content(result: ToolExecutionResult, content: Vec<ContentBlock>) -> ToolExecutionResult {
    match result {
        ToolExecutionResult::Success(mut success) => {
            success.content = content;
            ToolExecutionResult::Success(success)
        }
        ToolExecutionResult::Failure(mut failure) => {
            failure.content = content;
            ToolExecutionResult::Failure(failure)
        }
    }
}

/// Prepends deferred contexts to a result's contexts (dsh dispatch merge).
fn merge_deferred_contexts(
    result: ToolExecutionResult,
    deferred: Vec<Message>,
) -> ToolExecutionResult {
    if deferred.is_empty() {
        return result;
    }
    let mut contexts = deferred;
    contexts.extend(result.additional_contexts_to_owned());
    result.with_additional_contexts(contexts)
}

/// Extracts a printable message from a panic payload.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked".to_owned())
}

impl ToolExecutionResult {
    /// Returns the result with replaced appended contexts.
    #[must_use]
    pub fn with_additional_contexts(self, contexts: Vec<Message>) -> Self {
        match self {
            Self::Success(mut success) => {
                success.additional_contexts = contexts;
                Self::Success(success)
            }
            Self::Failure(mut failure) => {
                failure.additional_contexts = contexts;
                Self::Failure(failure)
            }
        }
    }
}
