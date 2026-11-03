//! The C-ABI dylib plugin shell (PRD 4.5): fr-compaction's artifact
//! surface over the dougong dynamic channel.
//!
//! Kernel-family edge: this module (and the [`HostLink`] wrapper) are
//! the only `dougong-abi` consumers; the rest of the crate stays a plain
//! rlib.
//!
//! The shell provides the `'compaction'` service (the 17h compact-now
//! arm pins its selector today; the describe face serves now) and
//! mounts the four automatic-compaction listeners as kernel listeners
//! (the dsh `ctx.on` faces; the 2026-11-02 ruling: each plugin
//! self-registers, the loop stays lean). Both waterfall listeners pass
//! through the CPS protocol: the dsh pre-step listener always calls
//! `next()` after its work, and the request-error listener either rules
//! `{kind: 'retry'}` (a short-circuit chain value) or defers like the
//! rest of the chain.
//!
//! The `auto` config gate mirrors the dsh registration gate: the kernel
//! delivers declared events unconditionally, so the handlers consult
//! the resolved flag where the dsh plugin would not have registered.
//!
//! The summary-error recovery hook dispatches `compaction/summary-error`
//! through the plugin-owned relay thread: an on-event (or call) foreign
//! entry may not originate a dispatch (deterministic
//! `STATUS_REENTRANCY`), so the recovery wait blocks on the relay's
//! reply while the dispatch runs on the relay's clean stack.
//!
//! Thread discipline: the relay thread is the only plugin-owned thread;
//! `teardown` joins it before returning (vtable calls after `teardown`
//! are documented undefined behavior).

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use dougong_abi::{
    ABI_VERSION, MODE_WATERFALL, PluginExports, STATUS_CAPACITY, STATUS_DEFERRED, STATUS_FAILED,
    STATUS_OK, write_out,
};
use fr_llm::error::CONTEXT_WINDOW_EXCEEDED_CODE;
use fr_llm::generation::{
    AbortRequest, BeginReply, BeginRequest, DrainReply, DrainRequest, WireGenerateOptions,
    WireModelInfo,
};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{ResolvedConfig, resolve_config, resolve_target_policy};
use crate::engine::{
    EngineDeps, EngineError, ModelCapacity, compact_if_needed, conversation_target, routed_target,
};
use crate::llm_face::{
    SummarizeStream, SummarizeTarget, resolve_summarization_target, summarize_with_llm,
};
use crate::meter::SurfaceMeasurement;
use crate::region::MeterView;
use crate::region::SessionView;
use crate::session_view::RemoteSessionView;
use crate::summarizer::{CompactionFailure, SummarizationInput, SummaryResult};

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-compaction";

/// The provided service name and its selectors (the shared protocol
/// table; the compact-now arm lands with the 17h batch).
pub use fr_wire::services::compaction::{METHOD_COMPACT_NOW, METHOD_DESCRIBE, SERVICE_NAME};

/// The listened event ids the descriptor's listen map binds.
pub const EV_PRE_STEP: u32 = 1;
/// The listened event ids the descriptor's listen map binds.
pub const EV_STATUS: u32 = 2;
/// The listened event ids the descriptor's listen map binds.
pub const EV_SESSION_EVENT: u32 = 3;
/// The listened event ids the descriptor's listen map binds.
pub const EV_REQUEST_ERROR: u32 = 4;

/// The descriptor the loader consumes: provides the `'compaction'`
/// service, injects the four consumed services, and listens for the four
/// automatic-compaction events. The version is sourced from crate
/// metadata so it cannot drift from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-compaction\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"compaction\"],\"inject\":[\"llm\",\"token-meter\",",
    "\"session-persistence\",\"agent\"],\"listen\":{\"agent/pre-step\":1,",
    "\"agent/status\":2,\"session/event\":3,\"agent/request-error\":4}}"
)
.as_bytes();

/// The consumer-side wire mirror of the `agent/pre-step` payload (the
/// dsh payload injects the agent; the wire names the session).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreStepMirror {
    session_id: String,
}

/// The consumer-side wire mirror of the `agent/status` payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusMirror {
    session_id: String,
    status: String,
}

/// The consumer-side wire mirror of the `session/event` payload: the
/// listener reads the event type only (the overflow bookkeeping arm).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEventMirror {
    session_id: String,
    event: serde_json::Value,
}

/// The consumer-side wire mirror of the `agent/request-error` payload's
/// failure facts (the overflow arm reads the code).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestErrorMirror {
    session_id: String,
    failure: FailureMirror,
}

/// The decoded failure facts the overflow arm consumes.
#[derive(Deserialize)]
struct FailureMirror {
    #[serde(default)]
    code: Option<String>,
}

/// The `compaction/summary-error` wire payload (the dsh
/// `{session, sourceEventSeqs, error, signal?}` shape; the signal has no
/// wire form — the recovery consumer's own cancellation rides its
/// caller).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SummaryErrorEvent<'a> {
    session_id: &'a str,
    source_event_seqs: Vec<u64>,
    error: FailureFacts<'a>,
}

/// The error facts the summary-error payload carries (the dsh thrown
/// `LlmError`: message, code, and the full provider failure when one
/// applies — the image-offload consumer reads `offloadImages` off it).
#[derive(serde::Serialize)]
struct FailureFacts<'a> {
    message: &'a str,
    code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<&'a fr_wire::Failure>,
}

/// The typed outbound seam (the batch-2a per-crate link judgment): one
/// copied vtable, callable from any plugin thread until `teardown`
/// returns.
#[derive(Debug, Clone, Copy)]
pub struct HostLink {
    host: dougong_abi::HostExports,
}

// SAFETY: the ABI vtable contract states every entry is callable from
// any plugin thread, concurrently, with host-side panic containment; the
// copy stays valid until this activation's `teardown` returns and the
// shell joins every thread holding a copy before then.
unsafe impl Send for HostLink {}
// SAFETY: sharing one copy across threads only ever issues concurrent
// vtable calls, which the same contract explicitly allows.
unsafe impl Sync for HostLink {}

impl HostLink {
    /// Wraps a vtable copied at `setup`.
    #[must_use]
    pub const fn new(host: dougong_abi::HostExports) -> Self {
        Self { host }
    }

    /// Performs one synchronous, blocking cross-plugin service call
    /// (the growable caller-owned buffer protocol).
    ///
    /// # Errors
    ///
    /// Returns the host status plus any JSON detail on a non-OK terminal
    /// status.
    pub fn require(&self, service: &str, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: the copied vtable is live until this activation's
            // `teardown` returns; every holder (the shell entries and the
            // relay thread) is joined before then.
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
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            if status == STATUS_CAPACITY {
                if cap >= MAX_REPLY_CAPACITY {
                    return Err(format!(
                        "outbound require failed with host status {status}: \
                         reply still exceeds {MAX_REPLY_CAPACITY} bytes"
                    ));
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            return Err(format!(
                "outbound require failed with host status {status}: {}",
                String::from_utf8_lossy(&buf)
            ));
        }
    }

    /// Performs one synchronous five-mode kernel dispatch and returns
    /// the settled chain value (the identity continuation when the chain
    /// stayed untouched).
    ///
    /// # Errors
    ///
    /// Returns the host status plus the JSON detail triple on a listener
    /// failure.
    pub fn dispatch(&self, event: &str, mode: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: same vtable lifetime and thread discipline as
            // `require`.
            let status = unsafe {
                (self.host.dispatch)(
                    self.host.host_handle,
                    event.as_ptr(),
                    event.len(),
                    mode,
                    payload.as_ptr(),
                    payload.len(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut len,
                )
            };
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            if status == STATUS_CAPACITY {
                if cap >= MAX_REPLY_CAPACITY {
                    return Err(format!(
                        "outbound dispatch failed with host status {status}: \
                         reply still exceeds {MAX_REPLY_CAPACITY} bytes"
                    ));
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            return Err(format!(
                "outbound dispatch failed with host status {status}: {}",
                String::from_utf8_lossy(&buf)
            ));
        }
    }
}

/// The host protocol's documented out-buffer ceiling (the loop shell's
/// 16 MiB judgment).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// One relay job: the dispatch the foreign entry cannot originate
/// itself, answered on the one-shot reply channel.
struct RelayJob {
    event: String,
    mode: u32,
    payload: Vec<u8>,
    reply: mpsc::Sender<Result<Vec<u8>, String>>,
}

/// The plugin-owned dispatch relay: the B6 ruling made concrete. Foreign
/// entries park a job on the channel; the relay thread (a clean
/// plugin-owned stack) originates the dispatch and answers with the
/// settled chain value.
struct Relay {
    tx: mpsc::Sender<RelayJob>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Relay {
    /// Spawns the relay thread over the copied link.
    #[must_use]
    fn spawn(link: HostLink) -> Self {
        let (tx, rx) = mpsc::channel::<RelayJob>();
        let handle = std::thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                let outcome = link.dispatch(&job.event, job.mode, &job.payload);
                let _ = job.reply.send(outcome);
            }
        });
        Self {
            tx,
            handle: Some(handle),
        }
    }

    /// Dispatches one event through the relay and waits for the settled
    /// chain value.
    ///
    /// # Errors
    ///
    /// Returns the host rejection verbatim (or the relay's own
    /// disconnect, which cannot happen before `teardown` joins).
    fn dispatch(&self, event: &str, mode: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(RelayJob {
                event: event.to_owned(),
                mode,
                payload: payload.to_vec(),
                reply: reply_tx,
            })
            .map_err(|error| format!("compaction dispatch relay is not running: {error}"))?;
        reply_rx
            .recv()
            .map_err(|error| format!("compaction dispatch relay dropped the job: {error}"))?
    }

    /// Joins the relay thread (the sender drops here, so the loop
    /// observes the disconnect and exits).
    fn join(&mut self) {
        self.tx = {
            let (tx, _rx) = mpsc::channel();
            tx
        };
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("live", &self.handle.is_some())
            .finish_non_exhaustive()
    }
}

/// The activation-scoped shell state. The host serializes every foreign
/// entry per instance, so the bookkeeping sets need no locking; the
/// relay thread touches only the copied link.
pub struct ShellState {
    /// The copied outbound seam the entries and the relay share.
    link: HostLink,
    /// The resolved compaction configuration (the dsh
    /// `BasicCompactionEngine.config`).
    pub config: ResolvedConfig,
    /// The pressure-config targets already warned once (the dsh
    /// `warnedPressureConfigTargets` set).
    warned_pressure_targets: HashSet<String>,
    /// The overflow retry counters per session (the dsh
    /// `overflowRetries` map; the dsh `WeakMap` dies with the session
    /// object, the fr map lives for the process — the 17f备案 deviation).
    overflow_retries: HashMap<String, u32>,
    /// The sessions whose latest failure was a context overflow (the dsh
    /// `overflowAgents` map keyed by session id).
    overflow_sessions: HashSet<String>,
    /// The dispatch relay (the B6 ruling's plugin-owned thread).
    relay: Relay,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("config", &self.config)
            .field("warned targets", &self.warned_pressure_targets.len())
            .field("overflow sessions", &self.overflow_sessions.len())
            .finish_non_exhaustive()
    }
}

impl ShellState {
    /// Assembles the shell over the activation's copied vtable with the
    /// default configuration (dougong delivers no config to dylib
    /// artifacts — the baked-config judgment; the assembly's overrides
    /// land with Phase 18).
    ///
    /// # Panics
    ///
    /// Panics when the default configuration refuses to resolve — a
    /// crate-level bug (the defaults are always valid).
    #[must_use]
    pub fn new(host: dougong_abi::HostExports) -> Self {
        let link = HostLink::new(host);
        let config = resolve_config(&serde_json::Value::Null)
            .expect("the default compaction configuration always resolves");
        Self::assembled(link, config)
    }

    /// Assembles the shell over explicit pieces (the tests drive the
    /// handlers without the loader).
    #[must_use]
    pub fn assembled(link: HostLink, config: ResolvedConfig) -> Self {
        Self {
            link,
            config,
            warned_pressure_targets: HashSet::new(),
            overflow_retries: HashMap::new(),
            overflow_sessions: HashSet::new(),
            relay: Relay::spawn(link),
        }
    }

    /// Joins the relay thread (the teardown discipline).
    pub fn join_relay(&mut self) {
        self.relay.join();
    }

    /// Opens the session view over the wire face (a full read per
    /// handling; compaction is rare and the transaction's appends
    /// reflect locally).
    fn open_session(&self, session_id: &str) -> Result<RemoteSessionView, String> {
        RemoteSessionView::open(&self.link, session_id)
    }
}

/// Serves one `'compaction'` service method invocation (the pure JSON
/// face so the shell surface stays testable). The compact-now arm lands
/// with the 17h batch.
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`.
pub fn dispatch(method: u32, payload: &[u8], _state: &mut ShellState) -> Result<Vec<u8>, String> {
    match method {
        METHOD_DESCRIBE => Ok(serde_json::to_vec(&serde_json::json!({
            "name": PLUGIN_NAME,
            "service": SERVICE_NAME,
            "abiVersion": ABI_VERSION,
        }))
        .unwrap_or_default()),
        METHOD_COMPACT_NOW => Err(format!("unknown compaction method {METHOD_COMPACT_NOW}")),
        _ => {
            let _ = payload;
            Err(format!("unknown compaction method {method}"))
        }
    }
}

/// Handles one `agent/pre-step` crossing (waterfall).
///
/// The dsh listener runs the pressure compaction between its abort
/// check and `next()`, warning once per misconfigured target; the fr
/// handler always defers — the chain value stays the loop's payload.
///
/// # Errors
///
/// Returns the failure text for a malformed payload (the loop's own
/// wire shape, so a decode failure is a loop-shell bug, not listener
/// work).
pub fn handle_pre_step(state: &mut ShellState, payload: &[u8]) -> Result<(), String> {
    let event: PreStepMirror = serde_json::from_slice(payload)
        .map_err(|error| format!("agent/pre-step payload is invalid: {error}"))?;
    if !state.config.auto {
        return Ok(());
    }
    match run_pressure_compaction(state, &event.session_id) {
        Ok(Some(result)) => {
            log_info(
                state,
                &format!(
                    "compaction (step pressure): shadowed {} surface nodes (seqs {}-{}, ~{} tokens)",
                    result.shadowed_seqs.len(),
                    result.shadowed_range.start.get(),
                    result.shadowed_range.end.get(),
                    result.shadowed_token_count,
                ),
            );
        }
        Ok(None) => {}
        Err(error) => match error.target_key() {
            Some(key) => {
                if state.warned_pressure_targets.insert(key.to_owned()) {
                    log_warn(
                        state,
                        &format!(
                            "step compaction failed: {}; continuing the turn",
                            error.message()
                        ),
                    );
                }
            }
            None => log_warn(
                state,
                &format!(
                    "step compaction failed: {}; continuing the turn",
                    error.message()
                ),
            ),
        },
    }
    Ok(())
}

/// Runs the pressure pipeline over the wire seams (the dsh
/// `compactIfNeeded(agent, 'pressure', signal)` call shape).
fn run_pressure_compaction(
    state: &ShellState,
    session_id: &str,
) -> Result<Option<crate::types::CompactionResult>, Box<EngineError>> {
    let mut view = state.open_session(session_id).map_err(EngineError::Plain)?;
    let meter = RemoteMeter {
        link: state.link,
        session_id: session_id.to_owned(),
    };
    let summarize = SummarizeHook::capture(state.link, &state.config, &view, session_id);
    let recover = RecoverHook {
        session_id: session_id.to_owned(),
        relay: &state.relay,
    };
    let capacity = CapacityHook { link: state.link };
    let mut deps = EngineDeps {
        meter: &meter,
        summarize: &mut |input| summarize.run(input),
        recover: &mut |failure, seqs| recover.run(failure, seqs),
        model_capacity: &mut |provider, model| capacity.run(provider, model),
    };
    compact_if_needed(
        &mut view,
        &mut deps,
        &state.config,
        crate::types::CompactionTrigger::Pressure,
    )
}

/// Handles one `agent/status` crossing (emit): an idle flip resets the
/// session's overflow retry sequence (the dsh `overflowRetries.delete`).
///
/// # Errors
///
/// Returns the failure text for a malformed payload.
pub fn handle_status(state: &mut ShellState, payload: &[u8]) -> Result<(), String> {
    let event: StatusMirror = serde_json::from_slice(payload)
        .map_err(|error| format!("agent/status payload is invalid: {error}"))?;
    if event.status == "idle" {
        state.overflow_retries.remove(&event.session_id);
    }
    Ok(())
}

/// Handles one `session/event` crossing (emit): a successful response
/// starts a fresh overflow-recovery sequence even when tool calls
/// continue the turn (the dsh `assistant/message` arm).
///
/// # Errors
///
/// Returns the failure text for a malformed payload.
pub fn handle_session_event(state: &mut ShellState, payload: &[u8]) -> Result<(), String> {
    let event: SessionEventMirror = serde_json::from_slice(payload)
        .map_err(|error| format!("session/event payload is invalid: {error}"))?;
    let event_type = event
        .event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type == "assistant/message" && state.overflow_sessions.contains(&event.session_id) {
        state.overflow_retries.remove(&event.session_id);
    }
    Ok(())
}

/// Handles one `agent/request-error` crossing (waterfall).
///
/// One provider-confirmed context overflow arms the recovery sequence —
/// the pipeline compacts past the threshold and rules `{kind: 'retry'}`
/// when durable surface progress landed, else preserves the original
/// error.
///
/// # Errors
///
/// Returns the failure text for a malformed payload or a pipeline
/// refusal the dsh listener would have propagated (the config
/// resolution throw).
pub fn handle_request_error(state: &mut ShellState, payload: &[u8]) -> Result<bool, String> {
    let event: RequestErrorMirror = serde_json::from_slice(payload)
        .map_err(|error| format!("agent/request-error payload is invalid: {error}"))?;
    if event.failure.code.as_deref() != Some(CONTEXT_WINDOW_EXCEEDED_CODE) {
        return Ok(false);
    }
    // The dsh listener's `signal.aborted` short-circuit has no wire
    // face: the loop driver polls its latch before dispatching, and a
    // cancel landing mid-dispatch cannot reach a foreign entry (the 17f
    // documented deviation; the auto path only).
    state.overflow_sessions.insert(event.session_id.clone());
    let mut view = state.open_session(&event.session_id)?;
    let Some((provider, model)) = routed_target(&view) else {
        return Ok(false);
    };
    let policy = resolve_target_policy(&state.config, &provider, &model);
    let retries = state
        .overflow_retries
        .get(&event.session_id)
        .copied()
        .unwrap_or(0);
    let retry_budget = u32::try_from(policy.max_overflow_retries.max(0)).unwrap_or(u32::MAX);
    if retries >= retry_budget {
        return Ok(false);
    }

    let meter = RemoteMeter {
        link: state.link,
        session_id: event.session_id.clone(),
    };
    let generation_before = view.replace_generation();
    let summarize = SummarizeHook::capture(state.link, &state.config, &view, &event.session_id);
    let recover = RecoverHook {
        session_id: event.session_id.clone(),
        relay: &state.relay,
    };
    let capacity = CapacityHook { link: state.link };
    let mut deps = EngineDeps {
        meter: &meter,
        summarize: &mut |input| summarize.run(input),
        recover: &mut |failure, seqs| recover.run(failure, seqs),
        model_capacity: &mut |provider, model| capacity.run(provider, model),
    };
    match compact_if_needed(
        &mut view,
        &mut deps,
        &state.config,
        crate::types::CompactionTrigger::ContextOverflow,
    ) {
        Ok(outcome) => {
            if view.replace_generation() <= generation_before {
                return Ok(false);
            }
            if let Some(result) = outcome {
                log_info(
                    state,
                    &format!(
                        "compaction (context overflow recovery): shadowed {} surface nodes \
                     (seqs {}-{}, ~{} tokens)",
                        result.shadowed_seqs.len(),
                        result.shadowed_range.start.get(),
                        result.shadowed_range.end.get(),
                        result.shadowed_token_count,
                    ),
                );
            }
            state.overflow_retries.insert(event.session_id, retries + 1);
            Ok(true)
        }
        Err(error) => {
            // A model-free prune can land before later summary work
            // fails; that durable reduction is sufficient retry proof
            // (the dsh comment's discipline — the 17g prune arm rides
            // the same generation check).
            if view.replace_generation() > generation_before {
                log_warn(
                    state,
                    &format!(
                        "context-overflow compaction failed after durable surface progress: {}; \
                     retrying from the replacement surface",
                        error.message()
                    ),
                );
                state.overflow_retries.insert(event.session_id, retries + 1);
                return Ok(true);
            }
            log_warn(
                state,
                &format!(
                    "context-overflow compaction failed: {}; preserving the original request error",
                    error.message()
                ),
            );
            Ok(false)
        }
    }
}

/// The summarizer hook (the dsh `summarize` override): the policy and
/// call targets, tool history, and the session id resolve at handler
/// entry — the dsh hook reads the live session, and the transaction
/// writes no tool-affecting event before summarization, so the captured
/// facts are identical values (the 17f frozen-facts judgment).
struct SummarizeHook {
    link: HostLink,
    /// The captured target facts; a capture failure surfaces exactly
    /// where a summarize-time failure would (the dsh hook throws inside
    /// the transaction, so the failed close leaves the paired end event
    /// — the frozen-facts capture preserves that trace).
    facts: Result<HookTargetFacts, CompactionFailure>,
}

/// The captured target facts one summarize hook carries.
struct HookTargetFacts {
    provider: String,
    model: String,
    max_tokens: i64,
    tool_history: Option<fr_wire::ToolHistory>,
    session_id: String,
}

impl SummarizeHook {
    /// Captures the hook's facts from the refreshed view (the dsh live
    /// reads: the policy for the conversation target, then the call
    /// target through the policy's summarization pair).
    fn capture(
        link: HostLink,
        config: &ResolvedConfig,
        view: &dyn SessionView,
        session_id: &str,
    ) -> Self {
        let agent = agent_route(&link, session_id).map_err(|error| {
            CompactionFailure::Other(format!("agent route lookup failed: {error}"))
        });
        let facts = agent.and_then(|agent| {
            let policy = conversation_target(
                view,
                agent
                    .as_ref()
                    .map(|route| (route.0.as_str(), route.1.as_str())),
            )
            .map(|(p, m)| resolve_target_policy(config, &p, &m));
            let (configured_provider, configured_model, max_tokens) = policy.as_ref().map_or_else(
                || {
                    (
                        config.summarization_provider.clone(),
                        config.summarization_model.clone(),
                        config.max_tokens,
                    )
                },
                |policy| {
                    (
                        policy.summarization_provider.clone(),
                        policy.summarization_model.clone(),
                        policy.max_tokens,
                    )
                },
            );
            let routed = view
                .request_header()
                .filter(|header| {
                    !header.config.provider.is_empty() && !header.config.model.is_empty()
                })
                .map(|header| (header.config.provider.clone(), header.config.model.clone()));
            let target = resolve_summarization_target(
                &configured_provider,
                &configured_model,
                routed.as_ref().map(|(p, _)| p.as_str()),
                routed.as_ref().map(|(_, m)| m.as_str()),
                agent.as_ref().map(|(p, _)| p.as_str()),
                agent.as_ref().map(|(_, m)| m.as_str()),
            )
            .map_err(CompactionFailure::Other)?;
            let mut projection = fr_session::ToolHistoryProjection::default();
            for event in view.events() {
                projection.apply(event);
            }
            Ok(HookTargetFacts {
                provider: target.provider,
                model: target.model,
                max_tokens,
                tool_history: Some(projection.snapshot()),
                session_id: session_id.to_owned(),
            })
        });
        Self { link, facts }
    }

    fn run(&self, input: &SummarizationInput) -> Result<SummaryResult, CompactionFailure> {
        let facts = match &self.facts {
            Ok(facts) => facts,
            Err(failure) => return Err(failure.clone()),
        };
        let mut stream = LlmStream { link: self.link };
        // The maintenance window's abort poll rides the token the 17h
        // compact-now wiring captures; the automatic paths have no
        // window and never abort mid-summarization.
        // The large-Err face boxes internally; the region seam keeps the
        // 17e-established plain failure type.
        summarize_with_llm(
            &mut stream,
            &SummarizeTarget {
                provider: facts.provider.clone(),
                model: facts.model.clone(),
            },
            facts.max_tokens,
            input,
            &facts.session_id,
            facts.tool_history.clone(),
            &mut || false,
        )
        .map_err(|error| *error)
    }
}

/// Reads the declared agent route (the dsh `agent.options` fallback; the
/// query face's additive route fields).
fn agent_route(link: &HostLink, session_id: &str) -> Result<Option<(String, String)>, String> {
    let request = serde_json::json!({ "sessionId": session_id });
    let reply = link.require(
        fr_wire::services::agent::SERVICE_NAME,
        fr_wire::services::agent::METHOD_STATUS,
        &serde_json::to_vec(&request).map_err(|error| error.to_string())?,
    )?;
    let reply: Value = serde_json::from_slice(&reply).map_err(|error| error.to_string())?;
    let provider = reply.get("provider").and_then(Value::as_str);
    let model = reply.get("model").and_then(Value::as_str);
    Ok(provider
        .zip(model)
        .map(|(p, m)| (p.to_owned(), m.to_owned())))
}

/// The summary-error recovery hook (the dsh `regionDependencies.recover`):
/// dispatches the waterfall through the relay and decodes the chain
/// value — the identity continuation means no listener answered
/// (recover `false`), a short-circuiting listener's value decodes as the
/// recovery ruling.
struct RecoverHook<'relay> {
    session_id: String,
    relay: &'relay Relay,
}

impl RecoverHook<'_> {
    fn run(&self, failure: &CompactionFailure, seqs: &[fr_wire::SessionSeq]) -> bool {
        let error = FailureFacts {
            message: failure.message(),
            code: match failure {
                CompactionFailure::Llm { code, .. } => code.as_deref(),
                _ => None,
            },
            failure: match failure {
                CompactionFailure::Llm { failure, .. } => failure.as_deref(),
                _ => None,
            },
        };
        let payload = SummaryErrorEvent {
            session_id: &self.session_id,
            source_event_seqs: seqs.iter().map(|seq| seq.get()).collect(),
            error,
        };
        let bytes = match serde_json::to_vec(&payload) {
            Ok(bytes) => bytes,
            Err(error) => {
                log_warn_global(&format!(
                    "compaction summary-error payload encode failed: {error}"
                ));
                return false;
            }
        };
        let reply = self
            .relay
            .dispatch(
                fr_wire::services::events::EVENT_SUMMARY_ERROR,
                MODE_WATERFALL,
                &bytes,
            )
            .unwrap_or_else(|error| {
                log_warn_global(&format!(
                    "compaction summary-error dispatch failed: {error}"
                ));
                bytes.clone()
            });
        if reply == bytes {
            return false;
        }
        let value: Value = match serde_json::from_slice(&reply) {
            Ok(value) => value,
            Err(_) => return false,
        };
        // The dsh `if (await recover(...))` truthiness: a JSON true (or
        // any truthy scalar the listener returns) rules recovery.
        match value {
            Value::Bool(flag) => flag,
            Value::Null | Value::Object(_) | Value::Array(_) => false,
            Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
            Value::String(text) => !text.is_empty(),
        }
    }
}

/// The capacity hook (the dsh `ctx.llm.resolveModelInfo` projection).
struct CapacityHook {
    link: HostLink,
}

impl CapacityHook {
    fn run(&self, provider: &str, model: &str) -> Result<ModelCapacity, String> {
        let request = serde_json::json!({ "provider": provider, "model": model });
        let reply = self.link.require(
            fr_wire::services::llm::SERVICE_NAME,
            fr_wire::services::llm::METHOD_RESOLVE_MODEL_INFO,
            &serde_json::to_vec(&request).map_err(|error| error.to_string())?,
        )?;
        let info: WireModelInfo = serde_json::from_slice(&reply)
            .map_err(|error| format!("model info decode: {error}"))?;
        Ok(ModelCapacity {
            context_window: info
                .context
                .as_ref()
                .and_then(|context| i64::try_from(context.context_window).ok()),
            default_max_tokens: info.default_max_tokens,
        })
    }
}

/// The LLM stream face over the `'llm'` service (the dsh
/// `ctx.llm.stream` pull trio).
struct LlmStream {
    link: HostLink,
}

impl SummarizeStream for LlmStream {
    fn resolve_model_info(
        &mut self,
        provider: &str,
        model: &str,
    ) -> Result<fr_llm::generation::WireModelInfo, String> {
        let request = serde_json::json!({ "provider": provider, "model": model });
        let reply = self.link.require(
            fr_wire::services::llm::SERVICE_NAME,
            fr_wire::services::llm::METHOD_RESOLVE_MODEL_INFO,
            &serde_json::to_vec(&request).map_err(|error| error.to_string())?,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("model info decode: {error}"))
    }

    fn begin(&mut self, request: WireGenerateOptions) -> Result<String, String> {
        let request = BeginRequest {
            request,
            prepared_id: None,
        };
        let reply = self.link.require(
            fr_wire::services::llm::SERVICE_NAME,
            fr_wire::services::llm::METHOD_BEGIN,
            &serde_json::to_vec(&request).map_err(|error| error.to_string())?,
        )?;
        let reply: BeginReply = serde_json::from_slice(&reply)
            .map_err(|error| format!("begin reply decode: {error}"))?;
        Ok(reply.generation_id)
    }

    fn drain(&mut self, generation: &str) -> Result<DrainReply, String> {
        let request = DrainRequest {
            generation_id: generation.to_owned(),
        };
        let reply = self.link.require(
            fr_wire::services::llm::SERVICE_NAME,
            fr_wire::services::llm::METHOD_DRAIN,
            &serde_json::to_vec(&request).map_err(|error| error.to_string())?,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("drain reply decode: {error}"))
    }

    fn abort(&mut self, generation: &str) -> Result<(), String> {
        let request = AbortRequest {
            generation_id: generation.to_owned(),
        };
        self.link.require(
            fr_wire::services::llm::SERVICE_NAME,
            fr_wire::services::llm::METHOD_ABORT,
            &serde_json::to_vec(&request).map_err(|error| error.to_string())?,
        )?;
        Ok(())
    }
}

/// The token-meter face over the `'token-meter'` service (the dsh
/// `ctx.tokenMeter` reads).
struct RemoteMeter {
    link: HostLink,
    session_id: String,
}

impl MeterView for RemoteMeter {
    fn measure(&self, session: &dyn SessionView) -> Result<SurfaceMeasurement, String> {
        let request = serde_json::json!({
            "id": self.session_id,
            "requestHeader": session.request_header(),
        });
        let reply = self.link.require(
            fr_wire::services::token_meter::SERVICE_NAME,
            fr_wire::services::token_meter::METHOD_MEASURE,
            &serde_json::to_vec(&request).map_err(|error| error.to_string())?,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("measurement decode: {error}"))
    }

    fn estimate_message(&self, message: &fr_wire::Message) -> u64 {
        let payload = serde_json::to_vec(&serde_json::json!({ "message": message }));
        let Ok(payload) = payload else {
            return 0;
        };
        let reply = self.link.require(
            fr_wire::services::token_meter::SERVICE_NAME,
            fr_wire::services::token_meter::METHOD_ESTIMATE_MESSAGE,
            &payload,
        );
        let Ok(reply) = reply else {
            return 0;
        };
        serde_json::from_slice::<serde_json::Value>(&reply)
            .ok()
            .and_then(|value| value.get("tokens").and_then(Value::as_u64))
            .unwrap_or(0)
    }
}

/// One informational compaction log line (the dsh `ctx.logger.info`).
fn log_info(_state: &ShellState, message: &str) {
    eprintln!("{message}");
}

/// One compaction warning (the dsh `ctx.logger.warn`).
fn log_warn(_state: &ShellState, message: &str) {
    eprintln!("{message}");
}

/// The relay-side warning sink (no shell borrow crosses the thread).
fn log_warn_global(message: &str) {
    eprintln!("{message}");
}

unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a `cap`-sized out-buffer and a writable
    // `len` slot (export-table contract).
    if unsafe { write_out(PLUGIN_NAME.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn descriptor(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: same host buffer contract as `plugin_name`.
    if unsafe { write_out(DESCRIPTOR, out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn setup(host: *const dougong_abi::HostExports) -> *mut std::os::raw::c_void {
    if host.is_null() {
        // Without the vtable the listeners cannot reach any consumed
        // service: fail the activation loudly instead of half-alive.
        return std::ptr::null_mut();
    }
    // SAFETY: the host exports pointer is valid for the duration of
    // `setup`; the vtable is copied by value (v3 contract).
    let exports = unsafe { *host };
    Box::into_raw(Box::new(ShellState::new(exports))).cast::<std::os::raw::c_void>()
}

unsafe extern "C" fn call(
    handle: *mut std::os::raw::c_void,
    method: u32,
    in_data: *const u8,
    in_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // SAFETY: the host passes back a handle previously returned by `setup`
    // and not yet released via `teardown`.
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    let reply = match dispatch(method, payload, state) {
        Ok(reply) => reply,
        Err(message) => {
            let body = serde_json::json!({ "error": message }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            return STATUS_FAILED;
        }
    };
    // SAFETY: host buffer contract.
    if unsafe { write_out(&reply, out, cap, out_len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

/// Serves one dispatched event: the four automatic-compaction listeners.
/// The waterfall pair either short-circuit (the chain value rides the
/// out-buffer) or defer (the host resolves the continuation into
/// `on_next`, which echoes the chain through).
unsafe extern "C" fn on_event(
    handle: *mut std::os::raw::c_void,
    event: u32,
    _mode: u32,
    in_data: *const u8,
    in_len: usize,
    next_available: u32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // SAFETY: live handle per the export-table contract.
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    let outcome: Result<Option<Vec<u8>>, String> = match event {
        EV_PRE_STEP => handle_pre_step(state, payload).map(|()| None),
        EV_STATUS => handle_status(state, payload).map(|()| None),
        EV_SESSION_EVENT => handle_session_event(state, payload).map(|()| None),
        EV_REQUEST_ERROR => handle_request_error(state, payload).map(|retry| {
            retry.then(|| {
                // The short-circuit chain value (the dsh
                // `{ kind: 'retry' }` ruling).
                br#"{"kind":"retry"}"#.to_vec()
            })
        }),
        _ => Err(format!("unknown compaction event {event}")),
    };
    match outcome {
        Ok(reply) => {
            if let Some(chain_value) = reply {
                // SAFETY: host buffer contract.
                if unsafe { write_out(&chain_value, out, cap, out_len) } {
                    return STATUS_OK;
                }
                return STATUS_CAPACITY;
            }
            if next_available == 1 {
                return STATUS_DEFERRED;
            }
            // SAFETY: host buffer contract; an empty JSON object reply.
            if unsafe { write_out(b"{}", out, cap, out_len) } {
                STATUS_OK
            } else {
                STATUS_CAPACITY
            }
        }
        Err(message) => {
            let body = serde_json::json!({ "error": message }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            STATUS_FAILED
        }
    }
}

/// Delivers one deferred waterfall continuation: the listener passes the
/// rest of the chain through (its output becomes the chain value here).
unsafe extern "C" fn on_next(
    handle: *mut std::os::raw::c_void,
    _event: u32,
    chain_status: i32,
    data: *const u8,
    len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() || chain_status != STATUS_OK {
        return STATUS_FAILED;
    }
    let chain: &[u8] = if len == 0 || data.is_null() {
        &[]
    } else {
        // SAFETY: the host passes one readable continuation payload per
        // the export-table contract.
        unsafe { std::slice::from_raw_parts(data, len) }
    };
    let _ = handle;
    // SAFETY: host buffer contract; the echo is the pass-through.
    if unsafe { write_out(chain, out, cap, out_len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn teardown(handle: *mut std::os::raw::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was produced by `setup` and torn down exactly once.
    let mut state = unsafe { Box::from_raw(handle.cast::<ShellState>()) };
    state.join_relay();
}

/// Export table for the fr-compaction dylib artifact; the host resolves
/// this symbol by name at load time.
#[unsafe(no_mangle)]
pub static DOUGONG_PLUGIN_EXPORTS: PluginExports = PluginExports {
    abi_version: ABI_VERSION,
    name: plugin_name,
    descriptor,
    setup,
    call,
    on_event,
    on_next,
    teardown,
};

#[cfg(test)]
mod tests {
    use super::{
        EV_PRE_STEP, EV_REQUEST_ERROR, EV_SESSION_EVENT, EV_STATUS, MODE_WATERFALL, ShellState,
    };
    use crate::config::resolve_config;
    use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_DEFERRED, STATUS_FAILED, STATUS_OK};
    use fr_session::EventVocabulary;
    use fr_session::service::{SessionService, dispatch as session_dispatch};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    const RETRY_CHAIN_VALUE: &[u8] = br#"{"kind":"retry"}"#;

    fn checkpoint_batch(text: &str) -> serde_json::Value {
        serde_json::json!({
            "generationId": "g1",
            "chunks": [
                { "type": "text-delta", "index": 0, "text": text },
                { "type": "finish", "reason": { "kind": "stop" } },
            ],
            "done": true,
        })
    }

    /// One full-fidelity host: `'session-persistence'` routes to a real
    /// [`SessionService`] over a temp root; `'token-meter'` answers the
    /// scripted measurement; `'llm'` answers the scripted model info and
    /// generation batches; `'agent'` answers the scripted route facts.
    struct TestHost {
        sessions: Mutex<SessionService>,
        llm_batches: Mutex<Vec<serde_json::Value>>,
        agent_route: Mutex<Option<(Option<String>, Option<String>)>>,
        agent_fails: AtomicBool,
        dispatch_log: Mutex<Vec<(String, Vec<u8>)>>,
    }

    /// Routes one `'session-persistence'` require to the real service.
    fn serve_sessions(
        state: &TestHost,
        method: u32,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut sessions = state.sessions.lock().expect("host session lock");
        match session_dispatch(method, payload.to_string().as_bytes(), &mut sessions) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(message) => Err(message),
        }
    }

    /// Routes one `'llm'` require to the scripted faces.
    fn serve_llm(state: &TestHost, method: u32) -> Result<serde_json::Value, String> {
        match method {
            fr_wire::services::llm::METHOD_RESOLVE_MODEL_INFO => Ok(serde_json::json!({
                "provider": "deepseek", "id": "m1", "name": "M1",
                "context": { "contextWindow": 262_144 },
                "defaultMaxTokens": 4_096,
            })),
            fr_wire::services::llm::METHOD_BEGIN => Ok(serde_json::json!({ "generationId": "g1" })),
            fr_wire::services::llm::METHOD_DRAIN => {
                let mut batches = state.llm_batches.lock().expect("host batches lock");
                let batch = if batches.is_empty() {
                    serde_json::json!({ "generationId": "g1", "chunks": [], "done": true })
                } else {
                    batches.remove(0)
                };
                Ok(batch)
            }
            fr_wire::services::llm::METHOD_ABORT => Ok(serde_json::json!({})),
            _ => Err(format!("unknown llm method {method}")),
        }
    }

    /// Routes one `'agent'` require to the scripted route facts.
    fn serve_agent(state: &TestHost) -> serde_json::Value {
        let route = state.agent_route.lock().expect("host route lock").clone();
        let mut reply = serde_json::json!({ "status": "idle" });
        if let Some((provider, model)) = route {
            if let Some(provider) = provider {
                reply["provider"] = serde_json::Value::String(provider);
            }
            if let Some(model) = model {
                reply["model"] = serde_json::Value::String(model);
            }
        }
        reply
    }

    /// Prices the live session dynamically (every surface node at 60000)
    /// so the measurement always matches the real surface.
    fn serve_measure(state: &TestHost, session_id: &str) -> serde_json::Value {
        let sessions = state.sessions.lock().expect("host session lock");
        let session = sessions
            .store()
            .get(&fr_wire::ids::SessionId::new(session_id))
            .expect("live session");
        let event_count = session.events().len() as u64;
        let surface = fr_session::fold_surface(session.events()).expect("surface fold");
        let nodes: Vec<serde_json::Value> = surface
            .nodes
            .iter()
            .map(|seq| {
                serde_json::json!({
                    "seq": seq.get(),
                    "tokens": 60_000,
                    "heuristicTokens": 60_000,
                })
            })
            .collect();
        let total = 60_000 * nodes.len() as u64;
        drop(sessions);
        serde_json::json!({
            "logRevision": event_count,
            "baseline": { "kind": "none", "tokens": 0 },
            "surfaceDeltaTokens": total,
            "totalTokens": total,
            "surfaceTokens": total,
            "nodes": nodes,
        })
    }

    /// One full-fidelity require router: session-persistence rides the
    /// real service, the meter prices the live session, llm and agent
    /// answer their scripts.
    unsafe extern "C" fn host_require(
        host: *mut c_void,
        service: *const u8,
        service_len: usize,
        method: u32,
        in_data: *const u8,
        in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: `host` is the `TestHost` the harness installed.
        let state = unsafe { &*host.cast::<TestHost>() };
        // SAFETY: slice-validity per the vtable contract.
        let service =
            unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(service, service_len)) };
        let payload: serde_json::Value = if in_len == 0 {
            serde_json::Value::Null
        } else {
            // SAFETY: same contract.
            match serde_json::from_slice(unsafe { std::slice::from_raw_parts(in_data, in_len) }) {
                Ok(value) => value,
                Err(error) => {
                    let bytes = serde_json::json!({ "error": error.to_string() })
                        .to_string()
                        .into_bytes();
                    // SAFETY: buffer protocol.
                    let _ = unsafe { dougong_abi::write_out(&bytes, out_buf, cap, out_len) };
                    return STATUS_FAILED;
                }
            }
        };
        let answer: Result<serde_json::Value, String> = match service.as_ref() {
            fr_wire::services::session_persistence::SERVICE_NAME => {
                serve_sessions(state, method, &payload)
            }
            fr_wire::services::token_meter::SERVICE_NAME => match method {
                fr_wire::services::token_meter::METHOD_MEASURE => {
                    let id = payload
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("s1");
                    Ok(serve_measure(state, id))
                }
                fr_wire::services::token_meter::METHOD_ESTIMATE_MESSAGE => {
                    Ok(serde_json::json!({ "tokens": 20 }))
                }
                _ => Err(format!("unknown token-meter method {method}")),
            },
            fr_wire::services::llm::SERVICE_NAME => serve_llm(state, method),
            fr_wire::services::agent::SERVICE_NAME => {
                if state.agent_fails.load(Ordering::Acquire) {
                    return STATUS_FAILED;
                }
                Ok(serve_agent(state))
            }
            _ => Err(format!("unknown service {service}")),
        };
        match answer {
            Ok(reply) => {
                let bytes = reply.to_string().into_bytes();
                // SAFETY: buffer protocol.
                if unsafe { dougong_abi::write_out(&bytes, out_buf, cap, out_len) } {
                    STATUS_OK
                } else {
                    STATUS_CAPACITY
                }
            }
            Err(message) => {
                let bytes = serde_json::json!({ "error": message })
                    .to_string()
                    .into_bytes();
                // SAFETY: buffer protocol.
                let _ = unsafe { dougong_abi::write_out(&bytes, out_buf, cap, out_len) };
                STATUS_FAILED
            }
        }
    }

    /// The dispatch double: records every crossing and answers the
    /// identity echo (an untouched chain — no summary-error listener).
    unsafe extern "C" fn host_dispatch(
        host: *mut c_void,
        event: *const u8,
        event_len: usize,
        _mode: u32,
        payload: *const u8,
        payload_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: `host` is the `TestHost` the harness installed.
        let state = unsafe { &*host.cast::<TestHost>() };
        // SAFETY: slice-validity per the vtable contract.
        let event =
            unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(event, event_len)) };
        // SAFETY: slice-validity per the vtable contract.
        let payload_bytes = unsafe { std::slice::from_raw_parts(payload, payload_len) }.to_vec();
        state
            .dispatch_log
            .lock()
            .expect("host dispatch lock")
            .push((event.into_owned(), payload_bytes.clone()));
        // SAFETY: buffer protocol (the identity echo).
        if unsafe { dougong_abi::write_out(&payload_bytes, out_buf, cap, out_len) } {
            STATUS_OK
        } else {
            STATUS_CAPACITY
        }
    }

    unsafe extern "C" fn host_grant(
        _host: *mut c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    unsafe extern "C" fn host_emit(
        _host: *mut c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fr-compaction-shell-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The harness shell over the real session store plus the scripted
    /// meter/llm/agent faces: one created session with an open turn,
    /// four priced messages, and a routed request header.
    fn harness(
        name: &str,
        llm_batches: Vec<serde_json::Value>,
        auto: bool,
    ) -> (Box<ShellState>, Arc<TestHost>) {
        let mut vocabulary = EventVocabulary::core();
        vocabulary
            .declare(&[
                "compaction/start".to_owned(),
                "compaction/summary".to_owned(),
                "compaction/end".to_owned(),
                "compaction/prune".to_owned(),
            ])
            .expect("compaction vocabulary declares");
        let mut sessions = {
            let clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| 1_000);
            let warn: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|_| {});
            SessionService::new(&temp_root(name), vocabulary, clock, warn).expect("service")
        };
        session_dispatch(
            fr_wire::services::session_persistence::METHOD_SESSION_CREATE,
            br#"{"id":"s1","meta":{"cwd":"/repo"}}"#,
            &mut sessions,
        )
        .expect("create");
        let mut seed = |body: serde_json::Value| {
            session_dispatch(
                fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
                body.to_string().as_bytes(),
                &mut sessions,
            )
            .expect("seed append");
        };
        seed(serde_json::json!({
            "id": "s1", "type": "turn/start", "data": { "turn": 1 }
        }));
        for index in 0..4 {
            seed(serde_json::json!({
                "id": "s1", "type": "user/message",
                "data": {
                    "role": "user",
                    "id": format!("usr-{index}"),
                    "content": [{ "type": "text", "text": format!("turn {index}") }],
                    "source": { "kind": "user" },
                },
                "surfaceOp": "append",
            }));
        }
        seed(serde_json::json!({
            "id": "s1", "type": "request/header",
            "data": {
                "header": {
                    "config": {
                        "provider": "deepseek",
                        "model": "m1",
                        "maxTokens": 1_024,
                    },
                },
                "reason": "initial",
            },
        }));

        let host = Arc::new(TestHost {
            sessions: Mutex::new(sessions),
            llm_batches: Mutex::new(llm_batches),
            agent_route: Mutex::new(Some((Some("deepseek".to_owned()), Some("m1".to_owned())))),
            agent_fails: AtomicBool::new(false),
            dispatch_log: Mutex::new(Vec::new()),
        });
        let exports = HostExports {
            grant: host_grant,
            dispatch: host_dispatch,
            host_handle: Arc::into_raw(Arc::clone(&host)).cast::<c_void>().cast_mut(),
            require: host_require,
            emit: host_emit,
        };
        let gate = if auto {
            serde_json::json!({ "auto": true })
        } else {
            // The auto default resolves true (the 17d port's default
            // registration); the inert fixture turns it off explicitly.
            serde_json::json!({ "auto": false })
        };
        let config = resolve_config(&gate).expect("harness config resolves");
        let state = Box::new(ShellState::assembled(super::HostLink::new(exports), config));
        (state, host)
    }

    /// Drives one `on_event` crossing through the shell's FFI entry,
    /// resolving a deferred continuation like the host does (the rest of
    /// the chain is empty, so the identity echoes back through
    /// `on_next`).
    fn drive(state: &mut ShellState, event: u32, payload: &[u8]) -> (i32, Vec<u8>) {
        let mut out = vec![0_u8; 4096];
        let mut len = 0_usize;
        // SAFETY: the handle aliases the live harness shell; the buffer
        // contract holds for the out slice and len slot.
        let status = unsafe {
            super::on_event(
                std::ptr::from_mut(state).cast::<c_void>(),
                event,
                MODE_WATERFALL,
                payload.as_ptr(),
                payload.len(),
                1,
                out.as_mut_ptr(),
                out.len(),
                &raw mut len,
            )
        };
        out.truncate(len);
        if status != STATUS_DEFERRED {
            return (status, out);
        }
        let mut chain = vec![0_u8; 4096];
        let mut chain_len = 0_usize;
        // SAFETY: same handle and buffer contract as the on_event drive.
        let resolved = unsafe {
            super::on_next(
                std::ptr::from_mut(state).cast::<c_void>(),
                event,
                STATUS_OK,
                payload.as_ptr(),
                payload.len(),
                chain.as_mut_ptr(),
                chain.len(),
                &raw mut chain_len,
            )
        };
        chain.truncate(chain_len);
        (resolved, chain)
    }

    fn event_types(host: &TestHost) -> Vec<String> {
        let sessions = host.sessions.lock().expect("host session lock");
        let session = sessions
            .store()
            .get(&fr_wire::ids::SessionId::new("s1"))
            .expect("live session");
        let types: Vec<String> = session
            .events()
            .iter()
            .map(|event| event.event_type().to_owned())
            .collect();
        drop(sessions);
        types
    }

    /// The automatic pre-step listener compacts the over-threshold
    /// session inside the open turn and defers like the rest of the
    /// chain (the dsh always-`next()` listener).
    #[test]
    fn pre_step_compacts_and_defers() {
        let (mut state, host) =
            harness("pressure", vec![checkpoint_batch("checkpoint body")], true);
        let pre_step_payload = br#"{"sessionId":"s1","messages":[],"turn":1,"step":1}"#.to_vec();
        let (status, reply) = drive(&mut state, EV_PRE_STEP, &pre_step_payload);
        assert_eq!(status, STATUS_OK, "the deferred crossing resolves");
        assert_eq!(
            reply, pre_step_payload,
            "the untouched chain echoes the payload (the identity continuation)"
        );
        let types = event_types(&host);
        assert!(types.contains(&"compaction/start".to_owned()));
        assert!(types.contains(&"compaction/summary".to_owned()));
        assert!(types.contains(&"compaction/end".to_owned()));
        state.join_relay();
    }

    /// `auto=false` keeps the listeners inert: the kernel may still
    /// deliver, but the handler compacts nothing (the dsh plugin would
    /// not have registered).
    #[test]
    fn auto_gate_keeps_the_listeners_inert() {
        let (mut state, host) = harness("inert", vec![], false);
        let (status, _reply) = drive(
            &mut state,
            EV_PRE_STEP,
            br#"{"sessionId":"s1","messages":[],"turn":1,"step":1}"#,
        );
        assert_eq!(status, STATUS_OK);
        let types = event_types(&host);
        assert!(
            !types.iter().any(|name| name.starts_with("compaction/")),
            "the gate-off handler compacts nothing: {types:?}"
        );
        state.join_relay();
    }

    /// The overflow recovery arm: the first context overflow compacts
    /// and rules the retry; the second spends the budget (the default
    /// max-overflow-retries of one) into the preserved-error arm.
    #[test]
    fn request_error_recovery_rules_retry_then_budget_exhausts() {
        let (mut state, _host) =
            harness("overflow", vec![checkpoint_batch("checkpoint body")], true);
        let failure =
            br#"{"sessionId":"s1","failure":{"message":"window","code":"CONTEXT_WINDOW_EXCEEDED"}}"#;
        let (status, reply) = drive(&mut state, EV_REQUEST_ERROR, failure);
        assert_eq!(status, STATUS_OK);
        assert_eq!(reply, RETRY_CHAIN_VALUE, "the recovery rules a retry");
        let (status, reply) = drive(&mut state, EV_REQUEST_ERROR, failure);
        assert_eq!(status, STATUS_OK);
        assert_eq!(
            reply, failure,
            "the second overflow preserves the original error (the identity continuation)"
        );
        state.join_relay();
    }

    /// The bookkeeping listeners: an idle status and an assistant
    /// response reset the session's overflow sequence, so a later
    /// overflow rules a retry again.
    #[test]
    fn bookkeeping_resets_the_overflow_sequence() {
        let (mut state, _host) = harness(
            "bookkeeping",
            vec![
                checkpoint_batch("first checkpoint"),
                checkpoint_batch("second checkpoint"),
            ],
            true,
        );
        let failure =
            br#"{"sessionId":"s1","failure":{"message":"window","code":"CONTEXT_WINDOW_EXCEEDED"}}"#;
        let (status, reply) = drive(&mut state, EV_REQUEST_ERROR, failure);
        assert_eq!(reply, RETRY_CHAIN_VALUE, "the first overflow rules a retry");
        let _ = status;
        // The assistant response resets the fresh sequence (the dsh
        // session/event arm) and the idle flip clears the counter.
        let (status, _reply) = drive(
            &mut state,
            EV_SESSION_EVENT,
            br#"{"sessionId":"s1","event":{"type":"assistant/message"}}"#,
        );
        assert_eq!(status, STATUS_OK);
        let (status, _reply) = drive(
            &mut state,
            EV_STATUS,
            br#"{"sessionId":"s1","status":"idle"}"#,
        );
        assert_eq!(status, STATUS_OK);
        let (status, reply) = drive(&mut state, EV_REQUEST_ERROR, failure);
        assert_eq!(status, STATUS_OK);
        assert_eq!(
            reply, RETRY_CHAIN_VALUE,
            "the reset sequence rules a retry again"
        );
        state.join_relay();
    }

    /// The pre-step listener with no resolvable summarization target
    /// fails inside the transaction: the paired failed close leaves
    /// start+end and no summary (the frozen-facts capture preserves the
    /// dsh in-transaction trace).
    #[test]
    fn capture_failure_leaves_the_paired_failed_close() {
        let (mut state, host) = harness("capture-fail", vec![], true);
        host.agent_fails.store(true, Ordering::Release);
        let (status, _reply) = drive(
            &mut state,
            EV_PRE_STEP,
            br#"{"sessionId":"s1","messages":[],"turn":1,"step":1}"#,
        );
        assert_eq!(status, STATUS_OK, "the always-defer listener resolves");
        let types = event_types(&host);
        assert!(types.contains(&"compaction/start".to_owned()));
        assert!(types.contains(&"compaction/end".to_owned()));
        assert!(
            !types.contains(&"compaction/summary".to_owned()),
            "the failed capture spends no summarization: {types:?}"
        );
        state.join_relay();
    }

    /// The summary-error recovery decodes the chain value: the identity
    /// echo means no listener answered (recover false), and the
    /// dispatched payload pins (session, source seqs, coded error, and
    /// the provider failure facts the offload consumer reads).
    #[test]
    fn summary_error_payload_pins_and_identity_means_no_recovery() {
        let (mut state, host) = harness("recover", vec![], true);
        // A failing capture inside the transaction routes the failure
        // through the recovery hook: the identity echo answers false,
        // and the dispatched summary-error payload pins.
        host.agent_fails.store(true, Ordering::Release);
        let _ = drive(
            &mut state,
            EV_PRE_STEP,
            br#"{"sessionId":"s1","messages":[],"turn":1,"step":1}"#,
        );
        let log = host.dispatch_log.lock().expect("dispatch log").clone();
        assert_eq!(log.len(), 1, "one summary-error crossing");
        assert_eq!(log[0].0, "compaction/summary-error");
        let payload: serde_json::Value = serde_json::from_slice(&log[0].1).expect("payload json");
        assert_eq!(payload["sessionId"], "s1");
        assert_eq!(
            payload["error"]["code"],
            serde_json::Value::Null,
            "a capture failure carries no code"
        );
        state.join_relay();
    }
}
