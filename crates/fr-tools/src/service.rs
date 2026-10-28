// Rust guideline compliant 2026-10-28
//! The `'tools'` service face over the dougong dynamic channel (Q101′A).
//!
//! Two halves live here. The dispatch surface mirrors the registry's
//! segmented scheduling face for the agent loop (prepare → dispatch →
//! settle pull → finalize/finish) plus the composite execute, wire
//! schemas, execution mode, and a token-keyed cancel (the wire carries no
//! signal object, so cancellation aborts the registry-stored caller
//! signal). The listener registration face mounts third-party policy
//! endpoints into the three decision stages; because a dylib plugin can
//! only answer synchronous calls, each endpoint is driven through the
//! two-leg continue-token protocol: the enter leg offers
//! delegate-or-final, a delegate runs the remaining chain in-process, and
//! the exit leg hands the downstream outcome back for the endpoint's
//! final say (decision Q101′A + F3: arm-deadline / continue / stop).
//!
//! Endpoint failure is fail-closed the way a throwing listener fails the
//! call in dsh: pre-execute denies, execute authors an error result,
//! post-execute blocks — policy is never bypassed by a dead endpoint.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_OK};
use fr_timeout::{AbortKind, DeadlineSignal};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{
    ApprovalOutcome, ApprovalSeam, ExecuteListener, ListenerRegistration, PostExecuteListener,
    PreExecuteListener, ToolRegistry, ToolsEmitter,
};
use crate::signal::ToolSignal;
use crate::types::{
    PostToolDecision, PreToolDecision, ToolExecution, ToolExecutionInput, ToolExecutionResult,
    ToolExecutionToken, text_content,
};

/// The service name this face provides.
pub const SERVICE_NAME: &str = "tools";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-tools";

/// Reports the shell metadata.
pub const METHOD_DESCRIBE: u32 = 0;
/// Mounts one policy endpoint into a decision stage.
pub const METHOD_REGISTER_LISTENER: u32 = 1;
/// Removes one mounted endpoint.
pub const METHOD_UNREGISTER_LISTENER: u32 = 2;
/// The concurrency mode of one call.
pub const METHOD_EXECUTION_MODE: u32 = 3;
/// Stage 1 of the segmented scheduling face.
pub const METHOD_PREPARE: u32 = 4;
/// Stage 2 (fast-path enqueue; the outcome settles asynchronously).
pub const METHOD_DISPATCH: u32 = 5;
/// Pulls the settlements that have arrived, in settle order.
pub const METHOD_SETTLED: u32 = 6;
/// Stage 3 for a post-result settlement.
pub const METHOD_FINALIZE: u32 = 7;
/// Stage 4 for a final-result settlement.
pub const METHOD_FINISH: u32 = 8;
/// The composite pipeline on the calling thread.
pub const METHOD_EXECUTE: u32 = 9;
/// The model-facing schema projection for the current mode.
pub const METHOD_WIRE_SCHEMAS: u32 = 10;
/// Aborts the caller signal of one in-flight call.
pub const METHOD_CANCEL: u32 = 11;
/// Installs or clears the wire approval endpoint (Q109-A).
///
/// `{endpoint: {service, method}}` installs; `{}` clears, restoring
/// the ask degrade. The approval provider's setup installs itself.
pub const METHOD_SET_APPROVAL: u32 = 12;

/// Mirror of `tools/change` (the tool set changed).
pub const EVENT_TOOLS_CHANGE: &str = "tools/change";
/// Mirror of `tools/result` (one frozen final result).
pub const EVENT_TOOLS_RESULT: &str = "tools/result";
/// Settle wake for the scheduler consumer (the `llm/chunk-ready` pattern
/// applied to the tools fabric: notify, then pull through `settled`).
pub const EVENT_TOOLS_SETTLED: &str = "tools/settled";

/// Upper bound for one endpoint or emit round trip on the growable
/// buffer protocol (the dougong host documents a 16 MiB ceiling).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// One copied host vtable with the two outbound arms this face uses
/// (require + emit; the fr-system-prompt `RequireSeam` judgment widened
/// with the emit entry the notification mirrors need).
#[derive(Debug, Clone, Copy)]
pub struct HostSeam {
    host: HostExports,
}

// SAFETY: the ABI vtable contract allows require/emit from any plugin
// thread, concurrently; the copy stays valid until this activation's
// `teardown` returns, and this face never outlives its activation.
unsafe impl Send for HostSeam {}

// SAFETY: sharing one copy only issues concurrent vtable calls, which the
// same contract explicitly allows.
unsafe impl Sync for HostSeam {}

impl HostSeam {
    /// Wraps a vtable copied at `setup`.
    #[must_use]
    pub const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Performs one synchronous cross-plugin call (growable buffer
    /// protocol; `STATUS_CAPACITY` doubles up to the 16 MiB ceiling).
    fn require(&self, service: &str, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: the copied vtable is live until this activation's
            // `teardown` returns; buffers are caller-owned per the
            // export-table contract.
            let status = unsafe {
                (self.host.require)(
                    self.host.host_handle,
                    service.as_ptr(),
                    service.len(),
                    method,
                    payload.as_ptr(),
                    payload.len(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut len,
                )
            };
            if status == STATUS_CAPACITY {
                if cap >= MAX_REPLY_CAPACITY {
                    return Err(format!(
                        "endpoint reply still exceeds {MAX_REPLY_CAPACITY} bytes"
                    ));
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            return Err(format!(
                "endpoint require {service}.{method} failed with host status {status}: {}",
                String::from_utf8_lossy(&buf)
            ));
        }
    }

    /// Publishes one fire-and-forget event to the kernel bus.
    fn emit(&self, event: &str, payload: &[u8]) -> Result<(), String> {
        // SAFETY: same vtable lifetime contract as `require`.
        let status = unsafe {
            (self.host.emit)(
                self.host.host_handle,
                event.as_ptr(),
                event.len(),
                payload.as_ptr(),
                payload.len(),
            )
        };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(format!("emit {event} failed with host status {status}"))
        }
    }
}

/// The registry notification bridge: mirrors `tools/change` and
/// `tools/result` onto the kernel bus once the seam attaches.
#[derive(Debug, Default)]
pub struct ToolEvents {
    seam: Mutex<Option<HostSeam>>,
}

impl ToolEvents {
    /// Attaches the outbound vtable (the activation-window seam; until it
    /// attaches the mirrors are silent, like an absent listener).
    ///
    /// # Panics
    ///
    /// Panics when the seam slot's lock is poisoned by a concurrent
    /// panic (the single-attach discipline keeps the uncontended path
    /// lock-clean).
    pub fn attach(&self, seam: HostSeam) {
        *self.seam.lock().expect("tool events lock") = Some(seam);
    }

    /// Best-effort mirror publish; listener-facing events never fail a
    /// pipeline path.
    fn publish(&self, event: &str, payload: &Value) {
        let guard = self.seam.lock().expect("tool events lock");
        if let Some(seam) = *guard {
            let bytes = serde_json::to_vec(payload).unwrap_or_default();
            // A failed mirror is logged by the kernel, never propagated.
            let _ = seam.emit(event, &bytes);
        }
    }
}

impl ToolsEmitter for ToolEvents {
    fn tools_change(&self) {
        self.publish(EVENT_TOOLS_CHANGE, &Value::Null);
    }

    fn tools_result(&self, exec: &ToolExecution, result: &ToolExecutionResult) {
        self.publish(
            EVENT_TOOLS_RESULT,
            &serde_json::json!({
                "call": wire_execution(exec),
                "result": result,
            }),
        );
    }
}

/// One failed service call (the `{"error": ...}` `STATUS_FAILED` body).
#[derive(Debug)]
pub struct ServiceError(pub String);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceError {}

/// The decision stages accepting endpoint registration (Q101′A); the
/// serde kebab-case forms are the dsh event names verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireStage {
    /// The pre-dispatch allow/deny/cancel/ask gate.
    PreExecute,
    /// The around-dispatch wrapper.
    Execute,
    /// The post-dispatch accept/replace/block gate.
    PostExecute,
}

impl WireStage {
    /// The stage name as it appears on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreExecute => "pre-execute",
            Self::Execute => "execute",
            Self::PostExecute => "post-execute",
        }
    }
}

/// The public identity of one registry-minted execution as it crosses the
/// wire (the signal stays registry-side, reachable through the token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireExecution {
    /// Registry-assigned identity (the cancel and settle key).
    pub token: ToolExecutionToken,
    /// Root model-requested call, resolved for every execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_call_id: Option<String>,
    /// Model-issued call identity.
    pub call_id: String,
    /// The tool name as requested.
    pub name: String,
    /// Binding-time schema, when a PTC inner call carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<crate::types::ToolSchema>,
    /// Frozen parsed arguments.
    pub arguments: Value,
    /// The agent on whose behalf the call runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Token of the enclosing transport execution, when nested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ToolExecutionToken>,
}

/// Projects one execution onto its wire form.
#[must_use]
pub fn wire_execution(exec: &ToolExecution) -> WireExecution {
    WireExecution {
        token: exec.token,
        root_call_id: Some(exec.root_call_id.clone()),
        call_id: exec.call_id.clone(),
        name: exec.name.clone(),
        schema: exec.schema.clone(),
        arguments: exec.arguments.clone(),
        agent: exec.agent.clone(),
        parent: exec.parent,
    }
}

/// The caller-supplied description of one call (the wire form of
/// [`ToolExecutionInput`]; the registry mints the signal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireCall {
    /// Model-issued call identity.
    pub call_id: String,
    /// Root model-requested call; `None` for a root execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_call_id: Option<String>,
    /// The tool name as requested.
    pub name: String,
    /// Binding-time tool schema for a PTC inner call (never logged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<crate::types::ToolSchema>,
    /// Parsed arguments (lossless JSON by construction).
    pub arguments: Value,
    /// The agent on whose behalf the call runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Token of the enclosing transport execution, when nested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ToolExecutionToken>,
}

impl WireCall {
    /// Rebuilds the registry input (a fresh caller signal; the registry
    /// stores it under the minted token for the cancel path).
    #[must_use]
    pub fn into_input(self) -> ToolExecutionInput {
        ToolExecutionInput {
            call_id: self.call_id,
            root_call_id: self.root_call_id,
            name: self.name,
            schema: self.schema,
            arguments: self.arguments,
            agent: self.agent,
            parent: self.parent,
            signal: ToolSignal::plain(DeadlineSignal::new()),
        }
    }
}

/// Stage 1 outcome: which stage the scheduler drives next.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WirePreparation {
    /// Run the around-dispatch and body stages.
    Dispatch {
        /// The minted execution.
        exec: WireExecution,
    },
    /// Skip dispatch; the result still receives post-execute.
    PostResult {
        /// The minted execution.
        exec: WireExecution,
        /// The pre-dispatch outcome.
        result: ToolExecutionResult,
    },
    /// Skip post-execute; the result already matches failure semantics.
    FinalResult {
        /// The minted execution.
        exec: WireExecution,
        /// The terminal outcome.
        result: ToolExecutionResult,
    },
}

/// Which scheduler stage one settled dispatch hands the consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireSettleKind {
    /// The result still receives post-execute (drive `finalize`).
    PostResult,
    /// The result already matches failure semantics (drive `finish`).
    FinalResult,
}

/// One settled dispatch, in settle order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSettle {
    /// The minted execution identity.
    pub token: ToolExecutionToken,
    /// Which scheduler stage the result belongs to.
    pub kind: WireSettleKind,
    /// The dispatch-stage outcome.
    pub result: ToolExecutionResult,
}

/// One registered callback endpoint (service + method selector).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireEndpoint {
    /// The endpoint service name.
    pub service: String,
    /// The endpoint method selector.
    pub method: u32,
}

/// The register-listener request.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterListenerRequest {
    /// The decision stage the endpoint joins.
    pub stage: WireStage,
    /// The callback endpoint.
    pub endpoint: WireEndpoint,
}

/// The register-listener reply.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterListenerReply {
    /// The registration id (the unregister key).
    pub id: u64,
}

/// The unregister-listener request.
#[derive(Debug, Serialize, Deserialize)]
pub struct UnregisterListenerRequest {
    /// The registration id being removed.
    pub id: u64,
}

/// The execution-mode request.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutionModeRequest {
    /// The tool name as requested.
    pub name: String,
    /// Parsed arguments.
    pub arguments: Value,
    /// Whether the call is a transport sub-dispatch.
    pub nested: bool,
}

/// The dispatch request (fast-path enqueue).
#[derive(Debug, Serialize, Deserialize)]
pub struct DispatchRequest {
    /// The minted execution identity.
    pub token: ToolExecutionToken,
}

/// The finalize request.
#[derive(Debug, Serialize, Deserialize)]
pub struct FinalizeRequest {
    /// The minted execution identity.
    pub token: ToolExecutionToken,
    /// The settled dispatch result.
    pub result: ToolExecutionResult,
}

/// The finish request.
#[derive(Debug, Serialize, Deserialize)]
pub struct FinishRequest {
    /// The minted execution identity.
    pub token: ToolExecutionToken,
    /// The terminal result.
    pub result: ToolExecutionResult,
}

/// The cancel request.
#[derive(Debug, Serialize, Deserialize)]
pub struct CancelRequest {
    /// The minted execution identity.
    pub token: ToolExecutionToken,
}

/// The set-approval request (Q109-A): one wire approval endpoint, or
/// none to restore the ask degrade.
#[derive(Debug, Deserialize)]
pub struct SetApprovalRequest {
    /// The endpoint to install; absent clears the seam.
    #[serde(default)]
    pub endpoint: Option<ApprovalEndpoint>,
}

/// One approval provider's request endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalEndpoint {
    /// The providing dougong service name.
    pub service: String,
    /// The method selector for the ask.
    pub method: u32,
}

/// The approval seam over one wire endpoint: the ask becomes a
/// synchronous require; every transport or decode failure resolves
/// `unavailable` (fail closed, the way a missing answerer fails the dsh
/// question) and never bypasses the ask.
struct WireApprovalSeam {
    seam: HostSeam,
    endpoint: ApprovalEndpoint,
}

impl ApprovalSeam for WireApprovalSeam {
    fn request(
        &self,
        exec: &ToolExecution,
        reason: Option<&str>,
        display_reason: Option<&str>,
    ) -> ApprovalOutcome {
        // The registry's ask gate already refused agent-less calls, so
        // the agent is present here by construction.
        let Some(agent) = exec.agent.as_deref() else {
            return ApprovalOutcome::Unavailable;
        };
        let payload = serde_json::json!({
            "agent": agent,
            "tool_name": exec.name,
            "call_id": exec.call_id.as_str(),
            "reason": reason,
            "display_reason": display_reason.map(|text| serde_json::json!({ "en": text })),
        });
        let Ok(bytes) = serde_json::to_vec(&payload) else {
            return ApprovalOutcome::Unavailable;
        };
        let Ok(reply) = self
            .seam
            .require(&self.endpoint.service, self.endpoint.method, &bytes)
        else {
            return ApprovalOutcome::Unavailable;
        };
        let Ok(parsed) = serde_json::from_slice::<Value>(&reply) else {
            return ApprovalOutcome::Unavailable;
        };
        match parsed.get("outcome").and_then(Value::as_str) {
            Some("allowed-once") => ApprovalOutcome::AllowedOnce,
            Some("rejected") => ApprovalOutcome::Rejected,
            Some("cancelled") => ApprovalOutcome::Cancelled,
            _ => ApprovalOutcome::Unavailable,
        }
    }
}

/// The cancel reply.
#[derive(Debug, Serialize, Deserialize)]
pub struct CancelReply {
    /// Whether the token named a live call.
    pub aborted: bool,
}

/// The deadline an execute-stage endpoint asks the adapter to arm (F3:
/// the signal object cannot cross the wire, so the guard sends its
/// parameters and the generic adapter does the arming).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmDeadline {
    /// The budget in milliseconds.
    pub ms: f64,
    /// The capability-owned classification code.
    pub code: String,
}

/// The per-service code interner.
///
/// fr-timeout reason codes are `&'static str` by design
/// (capability-owned constants), while the wire carries runtime text.
/// Each distinct code is leaked exactly once and cached, so growth is
/// bounded by the number of distinct policy codes the assembly ever
/// mounts (never per call).
pub type CodeInterner = Mutex<std::collections::HashMap<String, &'static str>>;

/// The longest code the interner accepts; longer text degrades to an
/// unarmed delegation instead of leaking unbounded bytes.
const MAX_CODE_BYTES: usize = 128;

/// Interns one wire code, or `None` when it is too long to trust.
fn intern_code(interner: &CodeInterner, code: &str) -> Option<&'static str> {
    let cached = {
        let map = interner.lock().expect("code interner lock");
        map.get(code).copied()
    };
    if let Some(static_code) = cached {
        return Some(static_code);
    }
    if code.len() > MAX_CODE_BYTES {
        return None;
    }
    let leaked: &'static str = Box::leak(code.to_owned().into_boxed_str());
    interner
        .lock()
        .expect("code interner lock")
        .insert(code.to_owned(), leaked);
    Some(leaked)
}

/// The final state of an armed deadline, reported on the exit leg.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadlineState {
    /// Whether the deadline fired at all.
    pub fired: bool,
    /// The timeout classification code, when a timeout (not an upstream
    /// cancel) won the first-abort race.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_code: Option<String>,
}

/// The leg of one two-leg endpoint conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireLeg {
    /// Before the remaining chain runs.
    Enter,
    /// After a delegate, carrying the downstream outcome.
    Exit,
}

/// The pre-execute enter leg.
#[derive(Debug, Serialize, Deserialize)]
pub struct PreEnter {
    /// The stage (always `pre-execute`).
    pub stage: WireStage,
    /// The leg (always `enter`).
    pub leg: WireLeg,
    /// The continue token pairing this conversation.
    pub token: u64,
    /// The pending call.
    pub call: WireExecution,
}

/// The pre-execute exit leg.
#[derive(Debug, Serialize, Deserialize)]
pub struct PreExit {
    /// The stage (always `pre-execute`).
    pub stage: WireStage,
    /// The leg (always `exit`).
    pub leg: WireLeg,
    /// The continue token pairing this conversation.
    pub token: u64,
    /// The remaining chain's decision.
    pub decision: PreToolDecision,
}

/// The pre-execute reply.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PreReply {
    /// Run the remaining chain, then report back on the exit leg.
    Delegate,
    /// Decide here; the remaining listeners are skipped.
    Final {
        /// This listener's decision.
        decision: PreToolDecision,
    },
}

/// The execute enter leg.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecEnter {
    /// The stage (always `execute`).
    pub stage: WireStage,
    /// The leg (always `enter`).
    pub leg: WireLeg,
    /// The continue token pairing this conversation.
    pub token: u64,
    /// The allowed call about to dispatch.
    pub call: WireExecution,
    /// The tool's declared timeout budget, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<f64>,
}

/// The execute exit leg.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecExit {
    /// The stage (always `execute`).
    pub stage: WireStage,
    /// The leg (always `exit`).
    pub leg: WireLeg,
    /// The continue token pairing this conversation.
    pub token: u64,
    /// The remaining chain's result (the body outcome when innermost).
    pub result: ToolExecutionResult,
    /// The armed deadline's final state, when one was armed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DeadlineState>,
}

/// The flavor of an authored execute outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireOutcome {
    /// The canonical result received from the continuation, unchanged.
    Canonical,
    /// An authored or modified result, normalized through the output
    /// contract.
    Authored,
}

/// The execute reply.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ExecReply {
    /// Run the remaining chain (optionally under an armed deadline), then
    /// report back on the exit leg.
    Delegate {
        /// The deadline the adapter should arm, when any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arm_deadline: Option<ArmDeadline>,
    },
    /// Decide here without running the remaining chain or the body.
    Final {
        /// The outcome flavor (authored results normalize through the
        /// output contract).
        outcome: WireOutcome,
        /// This listener's result.
        result: ToolExecutionResult,
    },
}

/// The post-execute enter leg.
#[derive(Debug, Serialize, Deserialize)]
pub struct PostEnter {
    /// The stage (always `post-execute`).
    pub stage: WireStage,
    /// The leg (always `enter`).
    pub leg: WireLeg,
    /// The continue token pairing this conversation.
    pub token: u64,
    /// The finished call.
    pub call: WireExecution,
    /// The dispatch-stage result under judgment.
    pub result: ToolExecutionResult,
}

/// The post-execute exit leg.
#[derive(Debug, Serialize, Deserialize)]
pub struct PostExit {
    /// The stage (always `post-execute`).
    pub stage: WireStage,
    /// The leg (always `exit`).
    pub leg: WireLeg,
    /// The continue token pairing this conversation.
    pub token: u64,
    /// The remaining chain's decision.
    pub decision: PostToolDecision,
}

/// The post-execute reply.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PostReply {
    /// Run the remaining chain, then report back on the exit leg.
    Delegate,
    /// Decide here; the remaining listeners are skipped.
    Final {
        /// This listener's decision.
        decision: PostToolDecision,
    },
}

/// Decodes one typed payload, mapping shape errors to the service
/// failure text.
fn decode<T: serde::de::DeserializeOwned>(payload: &[u8], entry: &str) -> Result<T, ServiceError> {
    serde_json::from_slice(payload)
        .map_err(|error| ServiceError(format!("tools {entry} payload is not valid: {error}")))
}

/// Encodes one reply.
fn encode<T: Serialize>(reply: &T) -> Result<Vec<u8>, ServiceError> {
    serde_json::to_vec(reply)
        .map_err(|error| ServiceError(format!("tools reply encoding failed: {error}")))
}

/// The fail-closed denial of a dead pre-execute endpoint.
fn endpoint_denial(stage: &str, service: &str, method: u32, text: &str) -> PreToolDecision {
    PreToolDecision::Deny {
        reason: format!("tools/{stage} listener {service}.{method} failed: {text}"),
        info: None,
    }
}

/// The fail-closed error result of a dead execute endpoint.
fn endpoint_error_result(text: &str) -> ToolExecutionResult {
    ToolExecutionResult::Failure(crate::types::ToolExecutionFailure {
        content: text_content(&format!("Error: {text}")),
        error: crate::types::ToolFailure {
            message: text.to_owned(),
            info: None,
        },
        meta: None,
        additional_contexts: Vec::new(),
    })
}

/// The fail-closed block of a dead post-execute endpoint.
fn endpoint_block(text: &str) -> PostToolDecision {
    PostToolDecision::Block {
        feedback: text_content(&format!("Error: {text}")),
        additional_contexts: Vec::new(),
    }
}

/// Calls one typed endpoint frame and decodes the typed reply.
fn call_endpoint<T: serde::de::DeserializeOwned>(
    seam: &HostSeam,
    endpoint: &WireEndpoint,
    frame: &impl Serialize,
) -> Result<T, String> {
    let payload =
        serde_json::to_vec(frame).map_err(|error| format!("listener frame encoding: {error}"))?;
    let reply = seam.require(&endpoint.service, endpoint.method, &payload)?;
    serde_json::from_slice(&reply).map_err(|error| format!("listener reply decode: {error}"))
}

/// Mounts one pre-execute endpoint adapter.
fn mount_pre_execute(
    seam: HostSeam,
    endpoint: WireEndpoint,
) -> impl Fn(&ToolExecution, &mut dyn FnMut() -> PreToolDecision) -> PreToolDecision + Send + Sync {
    let tokens = AtomicU64::new(1);
    move |exec, next| {
        let token = tokens.fetch_add(1, Ordering::Relaxed);
        let enter = PreEnter {
            stage: WireStage::PreExecute,
            leg: WireLeg::Enter,
            token,
            call: wire_execution(exec),
        };
        let reply: PreReply = match call_endpoint(&seam, &endpoint, &enter) {
            Ok(reply) => reply,
            Err(text) => {
                return endpoint_denial("pre-execute", &endpoint.service, endpoint.method, &text);
            }
        };
        match reply {
            PreReply::Final { decision } => decision,
            PreReply::Delegate => {
                let downstream = next();
                let exit = PreExit {
                    stage: WireStage::PreExecute,
                    leg: WireLeg::Exit,
                    token,
                    decision: downstream,
                };
                match call_endpoint(&seam, &endpoint, &exit) {
                    Ok(PreReply::Final { decision }) => decision,
                    Ok(PreReply::Delegate) => endpoint_denial(
                        "pre-execute",
                        &endpoint.service,
                        endpoint.method,
                        "exit leg must answer final",
                    ),
                    Err(text) => {
                        endpoint_denial("pre-execute", &endpoint.service, endpoint.method, &text)
                    }
                }
            }
        }
    }
}

/// Mounts one execute endpoint adapter (the generic arm-deadline
/// vocabulary of decision F3).
fn mount_execute(
    registry: Arc<ToolRegistry>,
    interner: Arc<CodeInterner>,
    seam: HostSeam,
    endpoint: WireEndpoint,
) -> impl Fn(
    &mut crate::registry::ToolDispatchView<'_>,
    &mut dyn FnMut(&mut crate::registry::ToolDispatchView<'_>) -> ToolExecutionResult,
) -> crate::registry::ToolExecuteOutcome
+ Send
+ Sync {
    let tokens = AtomicU64::new(1);
    move |view, next| {
        let token = tokens.fetch_add(1, Ordering::Relaxed);
        let timeout_ms = registry
            .get(&view.exec.name)
            .and_then(|tool| tool.timeout_ms());
        let enter = ExecEnter {
            stage: WireStage::Execute,
            leg: WireLeg::Enter,
            token,
            call: wire_execution(view.exec),
            timeout_ms,
        };
        let reply: ExecReply = match call_endpoint(&seam, &endpoint, &enter) {
            Ok(reply) => reply,
            Err(text) => {
                return crate::registry::ToolExecuteOutcome::Authored(endpoint_error_result(
                    &format!("tools/execute listener {} failed: {text}", endpoint.service),
                ));
            }
        };
        match reply {
            ExecReply::Final { outcome, result } => match outcome {
                WireOutcome::Canonical => crate::registry::ToolExecuteOutcome::Canonical(result),
                WireOutcome::Authored => crate::registry::ToolExecuteOutcome::Authored(result),
            },
            ExecReply::Delegate { arm_deadline } => {
                // The generic mechanism half of F3: build and arm the
                // deadline the endpoint asked for, swap the fused signal
                // in, delegate, restore, then report the deadline state.
                let armed = arm_deadline.and_then(|armed| {
                    let code = intern_code(&interner, &armed.code)?;
                    let deadline = fr_timeout::deadline(None, armed.ms, code).ok()?;
                    spawn_checker(deadline.clone(), armed.ms);
                    Some(deadline)
                });
                let upstream = Arc::clone(&view.signal);
                if let Some(deadline) = &armed {
                    let derived = ToolSignal::plain(deadline.signal().clone());
                    view.signal = ToolSignal::fuse(Arc::clone(&upstream), derived);
                }
                let result = next(view);
                view.signal = upstream;
                let deadline_state = armed.map(|deadline| {
                    let fired = deadline.check();
                    let timeout_code = deadline.signal().reason().and_then(|kind| match kind {
                        AbortKind::Timeout(reason) => Some(reason.code),
                        AbortKind::Upstream => None,
                    });
                    DeadlineState {
                        fired,
                        timeout_code: timeout_code.map(ToOwned::to_owned),
                    }
                });
                let exit = ExecExit {
                    stage: WireStage::Execute,
                    leg: WireLeg::Exit,
                    token,
                    result,
                    deadline: deadline_state,
                };
                match call_endpoint(&seam, &endpoint, &exit) {
                    Ok(ExecReply::Final { outcome, result }) => match outcome {
                        WireOutcome::Canonical => {
                            crate::registry::ToolExecuteOutcome::Canonical(result)
                        }
                        WireOutcome::Authored => {
                            crate::registry::ToolExecuteOutcome::Authored(result)
                        }
                    },
                    Ok(ExecReply::Delegate { .. }) => {
                        crate::registry::ToolExecuteOutcome::Authored(endpoint_error_result(
                            &format!(
                                "tools/execute listener {} failed: exit leg must answer final",
                                endpoint.service
                            ),
                        ))
                    }
                    Err(text) => {
                        crate::registry::ToolExecuteOutcome::Authored(endpoint_error_result(
                            &format!("tools/execute listener {} failed: {text}", endpoint.service),
                        ))
                    }
                }
            }
        }
    }
}

/// Arms the one-shot checker: a thread that sleeps the budget and then
/// advances the passive deadline so signal polls observe the expiry (the
/// fr-guard-timeout judgment; self-terminating right after its check).
fn spawn_checker(deadline: fr_timeout::Deadline, ms: f64) {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "JS Number parity: the timer consumes integer-truncated milliseconds"
    )]
    let millis = ms.max(0.0) as u64;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(millis));
        let _expired = deadline.check();
    });
}

/// Mounts one post-execute endpoint adapter.
fn mount_post_execute(
    seam: HostSeam,
    endpoint: WireEndpoint,
) -> impl Fn(
    &ToolExecution,
    &ToolExecutionResult,
    &mut dyn FnMut() -> PostToolDecision,
) -> PostToolDecision
+ Send
+ Sync {
    let tokens = AtomicU64::new(1);
    move |exec, result, next| {
        let token = tokens.fetch_add(1, Ordering::Relaxed);
        let enter = PostEnter {
            stage: WireStage::PostExecute,
            leg: WireLeg::Enter,
            token,
            call: wire_execution(exec),
            result: result.clone(),
        };
        let reply: PostReply = match call_endpoint(&seam, &endpoint, &enter) {
            Ok(reply) => reply,
            Err(text) => {
                return endpoint_block(&format!(
                    "tools/post-execute listener {} failed: {text}",
                    endpoint.service
                ));
            }
        };
        match reply {
            PostReply::Final { decision } => decision,
            PostReply::Delegate => {
                let downstream = next();
                let exit = PostExit {
                    stage: WireStage::PostExecute,
                    leg: WireLeg::Exit,
                    token,
                    decision: downstream,
                };
                match call_endpoint(&seam, &endpoint, &exit) {
                    Ok(PostReply::Final { decision }) => decision,
                    Ok(PostReply::Delegate) => endpoint_block(&format!(
                        "tools/post-execute listener {} failed: exit leg must answer final",
                        endpoint.service
                    )),
                    Err(text) => endpoint_block(&format!(
                        "tools/post-execute listener {} failed: {text}",
                        endpoint.service
                    )),
                }
            }
        }
    }
}

/// One mounted registration (dropping it deregisters).
struct ListenerSlot {
    id: u64,
    /// Kept alive for its `Drop` (the deregistration disposer).
    _registration: ListenerRegistration,
}

/// The service state behind the `'tools'` dynamic-channel face.
pub struct ToolsService {
    registry: Arc<ToolRegistry>,
    events: Arc<ToolEvents>,
    seam: Option<HostSeam>,
    listeners: Vec<ListenerSlot>,
    next_listener_id: u64,
    /// Executions minted by `prepare` and not yet finished (the token
    /// key the segmented methods share with cancel and settle).
    in_flight: Arc<Mutex<std::collections::HashMap<ToolExecutionToken, ToolExecution>>>,
    settles: Arc<Mutex<VecDeque<WireSettle>>>,
    workers: Vec<JoinHandle<()>>,
    interner: Arc<CodeInterner>,
}

impl std::fmt::Debug for ToolsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolsService")
            .field("seam", &self.seam)
            .field("listener_count", &self.listeners.len())
            .finish_non_exhaustive()
    }
}

impl ToolsService {
    /// Builds the service over one registry; `try_new` wires the
    /// notification mirrors, `attach` adds the outbound seam at setup.
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>, events: Arc<ToolEvents>) -> Self {
        Self {
            registry,
            events,
            seam: None,
            listeners: Vec::new(),
            next_listener_id: 1,
            in_flight: Arc::new(Mutex::new(std::collections::HashMap::new())),
            settles: Arc::new(Mutex::new(VecDeque::new())),
            workers: Vec::new(),
            interner: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Attaches the outbound vtable (activation window) and mounts it on
    /// the mirrors.
    pub fn attach(&mut self, seam: HostSeam) {
        self.events.attach(seam);
        self.seam = Some(seam);
    }

    /// Joins every in-flight dispatch worker (the teardown discipline:
    /// plugin-owned threads never outlive the activation).
    pub fn join_workers(&mut self) {
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }

    /// The hosted registry (tests drive registrations and tools).
    #[must_use]
    pub const fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    /// Serves one method invocation.
    ///
    /// # Errors
    ///
    /// Returns the failure the host surfaces as `STATUS_FAILED`.
    ///
    /// # Panics
    ///
    /// Panics when an internal state lock (in-flight, settle, interner)
    /// is poisoned by a concurrent panic; every lock guard is short-lived
    /// and panic-free on its own path.
    pub fn serve(&mut self, method: u32, payload: &[u8]) -> Result<Vec<u8>, ServiceError> {
        match method {
            METHOD_DESCRIBE => encode(&serde_json::json!({
                "name": PLUGIN_NAME,
                "service": SERVICE_NAME,
                "abiVersion": dougong_abi::ABI_VERSION,
            })),
            METHOD_REGISTER_LISTENER => self.serve_register_listener(payload),
            METHOD_UNREGISTER_LISTENER => self.serve_unregister_listener(payload),
            METHOD_EXECUTION_MODE => {
                let request: ExecutionModeRequest = decode(payload, "execution-mode")?;
                let mode =
                    self.registry
                        .execution_mode(&request.name, &request.arguments, request.nested);
                encode(&mode)
            }
            METHOD_PREPARE => self.serve_prepare(payload),
            METHOD_DISPATCH => {
                let request: DispatchRequest = decode(payload, "dispatch")?;
                let exec = self
                    .in_flight
                    .lock()
                    .expect("in-flight lock")
                    .get(&request.token)
                    .cloned();
                let Some(exec) = exec else {
                    return Err(ServiceError(format!(
                        "tools dispatch: unknown token {}",
                        request.token
                    )));
                };
                self.spawn_dispatch(exec);
                encode(&serde_json::json!({}))
            }
            METHOD_SETTLED => {
                let drained: Vec<WireSettle> = self
                    .settles
                    .lock()
                    .expect("settle lock")
                    .drain(..)
                    .collect();
                encode(&drained)
            }
            METHOD_FINALIZE => {
                let request: FinalizeRequest = decode(payload, "finalize")?;
                let exec = self.take_in_flight(request.token, "finalize")?;
                let result = self.registry.finalize_scheduled(&exec, request.result);
                encode(&result)
            }
            METHOD_FINISH => {
                let request: FinishRequest = decode(payload, "finish")?;
                let exec = self.take_in_flight(request.token, "finish")?;
                let result = self.registry.finish(&exec, request.result);
                encode(&result)
            }
            METHOD_EXECUTE => {
                let call: WireCall = decode(payload, "execute")?;
                let result = self.registry.execute(&call.into_input());
                encode(&result)
            }
            METHOD_WIRE_SCHEMAS => {
                let (schemas, known_names) = self.registry.wire_schemas().map_err(ServiceError)?;
                encode(&serde_json::json!({ "schemas": schemas, "knownNames": known_names }))
            }
            METHOD_CANCEL => {
                let request: CancelRequest = decode(payload, "cancel")?;
                let aborted = self.registry.abort_caller(request.token);
                encode(&CancelReply { aborted })
            }
            METHOD_SET_APPROVAL => {
                let request: SetApprovalRequest = decode(payload, "set-approval")?;
                match request.endpoint {
                    Some(endpoint) => {
                        let seam = self.seam.ok_or_else(|| {
                            ServiceError(
                                "tools set-approval requires the attached host seam".to_owned(),
                            )
                        })?;
                        self.registry
                            .set_approval(Some(Arc::new(WireApprovalSeam { seam, endpoint })));
                    }
                    None => self.registry.set_approval(None),
                }
                encode(&serde_json::json!({}))
            }
            other => Err(ServiceError(format!(
                "unknown {PLUGIN_NAME} method {other}"
            ))),
        }
    }

    /// Stage 1: mint the execution, track it under its token, and answer
    /// the public identity (the live caller signal stays registry-side).
    ///
    /// # Panics
    ///
    /// Panics when the in-flight lock is poisoned by a concurrent panic.
    fn serve_prepare(&self, payload: &[u8]) -> Result<Vec<u8>, ServiceError> {
        let call: WireCall = decode(payload, "prepare")?;
        let preparation = self.registry.prepare_scheduled(&call.into_input());
        let (wired, token, exec) = match preparation {
            crate::registry::ScheduledPreparation::Dispatch(exec) => (
                WirePreparation::Dispatch {
                    exec: wire_execution(&exec),
                },
                exec.token,
                exec,
            ),
            crate::registry::ScheduledPreparation::PostResult(exec, result) => (
                WirePreparation::PostResult {
                    exec: wire_execution(&exec),
                    result,
                },
                exec.token,
                exec,
            ),
            crate::registry::ScheduledPreparation::FinalResult(exec, result) => (
                WirePreparation::FinalResult {
                    exec: wire_execution(&exec),
                    result,
                },
                exec.token,
                exec,
            ),
        };
        self.in_flight
            .lock()
            .expect("in-flight lock")
            .insert(token, exec);
        encode(&wired)
    }

    fn serve_register_listener(&mut self, payload: &[u8]) -> Result<Vec<u8>, ServiceError> {
        let request: RegisterListenerRequest = decode(payload, "register-listener")?;
        let Some(seam) = self.seam else {
            return Err(ServiceError(
                "tools register-listener: no host seam attached".to_owned(),
            ));
        };
        let registration = match request.stage {
            WireStage::PreExecute => {
                let listener: PreExecuteListener =
                    Arc::new(mount_pre_execute(seam, request.endpoint));
                self.registry.on_pre_execute(listener)
            }
            WireStage::Execute => {
                let listener: ExecuteListener = Arc::new(mount_execute(
                    Arc::clone(&self.registry),
                    Arc::clone(&self.interner),
                    seam,
                    request.endpoint,
                ));
                self.registry.on_execute(listener)
            }
            WireStage::PostExecute => {
                let listener: PostExecuteListener =
                    Arc::new(mount_post_execute(seam, request.endpoint));
                self.registry.on_post_execute(listener)
            }
        };
        let id = self.next_listener_id;
        self.next_listener_id += 1;
        self.listeners.push(ListenerSlot {
            id,
            _registration: registration,
        });
        encode(&RegisterListenerReply { id })
    }

    fn serve_unregister_listener(&mut self, payload: &[u8]) -> Result<Vec<u8>, ServiceError> {
        let request: UnregisterListenerRequest = decode(payload, "unregister-listener")?;
        let index = self.listeners.iter().position(|slot| slot.id == request.id);
        match index {
            Some(index) => {
                // Dropping the slot drops the registration disposer, which
                // deregisters from the stage list.
                self.listeners.remove(index);
                encode(&serde_json::json!({}))
            }
            None => Err(ServiceError(format!(
                "tools unregister-listener: unknown id {}",
                request.id
            ))),
        }
    }

    /// Spawns one dispatch worker (the slow-work invariant: the service
    /// method returns immediately, the pipeline runs on a plugin-owned
    /// thread, the settle queue plus the `tools/settled` event carry the
    /// outcome back).
    fn spawn_dispatch(&mut self, exec: ToolExecution) {
        let registry = Arc::clone(&self.registry);
        let settles = Arc::clone(&self.settles);
        let seam = self.seam;
        let token = exec.token;
        let worker = std::thread::Builder::new()
            .name("tools-dispatch".to_owned())
            .spawn(move || {
                let settle = match registry.dispatch_scheduled(&exec) {
                    Ok(crate::registry::ScheduledDispatch::PostResult(result)) => WireSettle {
                        token,
                        kind: WireSettleKind::PostResult,
                        result,
                    },
                    Ok(crate::registry::ScheduledDispatch::FinalResult(result)) => WireSettle {
                        token,
                        kind: WireSettleKind::FinalResult,
                        result,
                    },
                    Err(result) => WireSettle {
                        token,
                        kind: WireSettleKind::FinalResult,
                        result: *result,
                    },
                };
                settles.lock().expect("settle lock").push_back(settle);
                if let Some(seam) = seam {
                    let bytes = serde_json::to_vec(&serde_json::json!({ "token": token }))
                        .unwrap_or_default();
                    // A failed wake is logged by the kernel; the consumer
                    // also polls settles at its step boundaries.
                    let _ = seam.emit(EVENT_TOOLS_SETTLED, &bytes);
                }
            })
            .map_err(|error| ServiceError(format!("tools dispatch spawn: {error}")));
        if let Ok(worker) = worker {
            self.workers.push(worker);
        }
    }

    fn take_in_flight(
        &self,
        token: ToolExecutionToken,
        entry: &str,
    ) -> Result<ToolExecution, ServiceError> {
        self.in_flight
            .lock()
            .expect("in-flight lock")
            .remove(&token)
            .ok_or_else(|| ServiceError(format!("tools {entry}: unknown token {token}")))
    }
}
