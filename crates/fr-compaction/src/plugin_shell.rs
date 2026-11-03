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
//! Thread discipline: the relay and the startup sweep are the only
//! plugin-owned threads; `teardown` joins them before returning (vtable
//! calls after `teardown` are documented undefined behavior).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::mpsc;

use dougong_abi::{
    ABI_VERSION, MODE_WATERFALL, PluginExports, STATUS_CAPACITY, STATUS_DEFERRED, STATUS_FAILED,
    STATUS_OK, write_out,
};
use fr_llm::error::{CONTEXT_WINDOW_EXCEEDED_CODE, IMAGE_OFFLOAD_REQUIRED_CODE};
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
use crate::spill::SaveTextSpill;
use crate::spill_cleanup::{gather_sweep_roots, sweep_spill_roots};
use crate::spill_local::LocalSpillStore;
use crate::spill_policy::{ImagePrice, SpillSeams, bound as spill_bound, is_image};
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

/// The descriptor the loader consumes: provides the `'compaction'` and
/// `'spill-store'` services (the fr-sandbox two-service precedent), injects
/// the consumed services (plus the `tools` face the spill endpoints
/// register into; the attachments and fs reads stay soft — the dsh
/// `ctx.get` absence arms), and listens for the four automatic-compaction
/// events. The version is sourced from crate metadata so it cannot drift
/// from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-compaction\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"compaction\",\"spill-store\"],\"inject\":[\"llm\",",
    "\"token-meter\",\"session-persistence\",\"agent\",\"tools\"],",
    "\"listen\":{\"agent/pre-step\":1,\"agent/status\":2,\"session/event\":3,",
    "\"agent/request-error\":4}}"
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
/// failure facts (the overflow arm reads the code; the image-offload
/// arm reads the offload count).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestErrorMirror {
    session_id: String,
    failure: FailureMirror,
}

/// The decoded failure facts the request-error arms consume.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FailureMirror {
    #[serde(default)]
    code: Option<String>,
    /// With code `IMAGE_OFFLOAD_REQUIRED`: how many more of the oldest
    /// retained occurrences the route needs offloaded.
    #[serde(default)]
    offload_images: Option<u64>,
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

    /// Grants `target`'s provider the right to require `granted` (D31;
    /// the callback-endpoint authorization the spill registration uses).
    ///
    /// # Errors
    ///
    /// The host refusal status.
    pub fn grant(&self, target: &str, granted: &str) -> Result<(), String> {
        // SAFETY: the copied vtable is live until this activation's
        // `teardown` returns; both name buffers live for the call.
        let status = unsafe {
            (self.host.grant)(
                self.host.host_handle,
                target.as_ptr(),
                target.len(),
                granted.as_ptr(),
                granted.len(),
            )
        };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(format!(
                "grant {granted} to {target} failed with host status {status}"
            ))
        }
    }

    /// Performs one synchronous, blocking cross-plugin service call
    /// (the growable caller-owned buffer protocol).
    ///
    /// # Errors
    ///
    /// Returns the host status plus any JSON detail on a non-OK terminal
    /// status.
    pub fn require(&self, service: &str, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        self.require_with_initial(service, method, payload, INITIAL_REPLY_CAPACITY)
    }

    /// The require leg over an explicit initial capacity. A
    /// `STATUS_CAPACITY` retry re-executes the service method (the
    /// kernel's `require_impl` runs the invocation once per call), so a
    /// mutating call must start large enough to fit its reply in the
    /// first pass — the append face sizes from its request length.
    ///
    /// # Errors
    ///
    /// Returns the host status plus any JSON detail on a non-OK terminal
    /// status.
    pub fn require_with_initial(
        &self,
        service: &str,
        method: u32,
        payload: &[u8],
        initial_capacity: usize,
    ) -> Result<Vec<u8>, String> {
        let mut cap = initial_capacity.max(INITIAL_REPLY_CAPACITY);
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

/// The default first-pass reply capacity for idempotent reads.
const INITIAL_REPLY_CAPACITY: usize = 4096;

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
    /// The local spill backend (the dsh `spill-local` plugin folded into
    /// the one crate; the spill endpoints read it in-process — a
    /// self-instance require is the deterministic reentrancy refusal).
    spill: LocalSpillStore,
    /// The startup sweep thread (the dsh activation effect; joined at
    /// teardown). `None` when cleanup is disabled (`0` days).
    sweep: Option<std::thread::JoinHandle<()>>,
    /// The spill conversations awaiting their exit legs, keyed by the
    /// continue token (the enter facts the compose step reads).
    pending_spill: Mutex<HashMap<u64, SpillEnterFacts>>,
}

/// The enter-leg facts one spill conversation composes against (the dsh
/// listener closure's captured `exec` and `result`).
#[derive(Debug, Clone)]
struct SpillEnterFacts {
    /// The finished call's session owner (`agent`).
    agent: Option<String>,
    /// The tool name as requested.
    name: String,
    /// The model-issued call identity.
    call_id: String,
    /// Token of the enclosing transport execution, when nested.
    parent: Option<String>,
    /// Whether the dispatch-stage result failed.
    is_error: bool,
    /// The dispatch-stage result content (the `decision.content ??
    /// result.content` fallback).
    content: Vec<fr_wire::ContentBlock>,
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
    /// handlers without the loader). The spill backend roots at the
    /// configured root or the per-process private default; the startup
    /// sweep launches off-thread (the dsh activation effect: service
    /// availability is never delayed) when cleanup is enabled.
    ///
    /// # Panics
    ///
    /// Panics when the sweep thread cannot spawn (thread-exhaustion is
    /// an activation-host failure, not a policy state).
    #[must_use]
    pub fn assembled(link: HostLink, config: ResolvedConfig) -> Self {
        let spill = config
            .spill_root
            .as_ref()
            .map_or_else(LocalSpillStore::private, |root| {
                LocalSpillStore::rooted(std::path::PathBuf::from(root))
            });
        let sweep = (config.spill_cleanup_period_days > 0).then(|| {
            let root = spill.root().to_path_buf();
            let days = config.spill_cleanup_period_days;
            std::thread::Builder::new()
                .name("spill-sweep".to_owned())
                .spawn(move || run_startup_sweep(&root, days))
                .expect("spill sweep thread spawns")
        });
        Self {
            link,
            config,
            warned_pressure_targets: HashSet::new(),
            overflow_retries: HashMap::new(),
            overflow_sessions: HashSet::new(),
            relay: Relay::spawn(link),
            spill,
            sweep,
            pending_spill: Mutex::new(HashMap::new()),
        }
    }

    /// Joins the relay thread (the teardown discipline).
    pub fn join_relay(&mut self) {
        self.relay.join();
    }

    /// Joins every plugin-owned thread (the teardown discipline).
    pub fn join_threads(&mut self) {
        self.relay.join();
        if let Some(sweep) = self.sweep.take() {
            let _joined = sweep.join();
        }
    }

    /// Registers the spill endpoints into the `tools` decision stages
    /// (the dsh `ctx.on(..., {prepend: true})` pair; the Q101′A
    /// endpoint registration). The grant authorizes the tools face to
    /// call back into the `compaction` service; registration declining
    /// leaves the spill policy inert while the rest of the plugin stays
    /// alive (the dsh unmounted-plugin equivalence). No-op when the
    /// policy is disabled (`max-inline-tokens` omitted).
    ///
    /// # Errors
    ///
    /// The grant or registration transport refusal (the caller decides
    /// between activation abort and the inert degrade).
    pub fn register_spill_endpoints(&mut self) -> Result<(), String> {
        if self.config.spill_max_inline_tokens.is_none() {
            return Ok(());
        }
        self.link.grant("tools", "compaction")?;
        for stage in ["post-execute", "dispatch-log"] {
            let registration = serde_json::json!({
                "stage": stage,
                "endpoint": {
                    "service": "compaction",
                    "method": if stage == "post-execute" {
                        fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE
                    } else {
                        fr_wire::services::compaction::METHOD_SPILL_DISPATCH_LOG
                    },
                },
            });
            let payload = serde_json::to_vec(&registration)
                .map_err(|error| format!("spill registration encode failed: {error}"))?;
            self.link.require("tools", 1, &payload)?;
        }
        Ok(())
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
pub fn dispatch(method: u32, payload: &[u8], state: &mut ShellState) -> Result<Vec<u8>, String> {
    match method {
        METHOD_DESCRIBE => Ok(serde_json::to_vec(&serde_json::json!({
            "name": PLUGIN_NAME,
            "service": SERVICE_NAME,
            "abiVersion": ABI_VERSION,
        }))
        .unwrap_or_default()),
        METHOD_COMPACT_NOW => Err(format!("unknown compaction method {METHOD_COMPACT_NOW}")),
        fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE => {
            serve_spill_endpoint(state, payload, SpillArm::PostExecute)
        }
        fr_wire::services::compaction::METHOD_SPILL_DISPATCH_LOG => {
            serve_spill_endpoint(state, payload, SpillArm::DispatchLog)
        }
        fr_wire::services::spill_store::METHOD_SAVE_TEXT => {
            let save: SaveTextSpill = serde_json::from_slice(payload)
                .map_err(|error| format!("save-text payload is invalid: {error}"))?;
            let reference = crate::spill::SpillStore::save_text(&state.spill, &save)?;
            serde_json::to_vec(&reference)
                .map_err(|error| format!("save-text reply encode failed: {error}"))
        }
        _ => {
            let _ = payload;
            Err(format!("unknown compaction method {method}"))
        }
    }
}

/// The spill endpoint arm a frame belongs to (the Q101′A segment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpillArm {
    /// The `tools/post-execute` callback.
    PostExecute,
    /// The `tools/ptc-dispatch-log` callback.
    DispatchLog,
}

/// Serves one spill two-leg frame (the dsh listener bodies): the enter
/// leg always defers (the dsh listener calls `next()` first); the exit
/// leg composes the listener's answer over the downstream decision.
///
/// # Errors
///
/// The malformed-frame refusal (the caller's wire shape, so a decode
/// failure is a tools-shell bug, not listener work).
fn serve_spill_endpoint(
    state: &ShellState,
    payload: &[u8],
    arm: SpillArm,
) -> Result<Vec<u8>, String> {
    let frame: Value = serde_json::from_slice(payload)
        .map_err(|error| format!("spill frame is invalid: {error}"))?;
    match frame.get("leg").and_then(Value::as_str) {
        Some("enter") => {
            let enter: SpillEnterMirror = serde_json::from_value(frame)
                .map_err(|error| format!("spill enter frame is invalid: {error}"))?;
            let facts = SpillEnterFacts {
                agent: enter.call.agent,
                name: enter.call.name,
                call_id: enter.call.call_id,
                parent: enter.call.parent,
                is_error: enter.result.is_error(),
                content: enter.result.content(),
            };
            state
                .pending_spill
                .lock()
                .expect("pending spill lock")
                .insert(enter.token, facts);
            serde_json::to_vec(&serde_json::json!({ "kind": "delegate" }))
                .map_err(|error| format!("spill reply encode failed: {error}"))
        }
        Some("exit") => match arm {
            SpillArm::PostExecute => serve_spill_post_execute_exit(state, &frame),
            SpillArm::DispatchLog => serve_spill_dispatch_log_exit(state, &frame),
        },
        other => Err(format!("spill frame leg is invalid: {other:?}")),
    }
}

/// The `tools/post-execute` exit leg (the dsh post-execute listener
/// body): the gate skips non-accept decisions, value replacements, and
/// the read tool; the PTC-parent text-only arm rides the dispatch-log
/// path; everything else bounds and composes the replacement.
fn serve_spill_post_execute_exit(state: &ShellState, frame: &Value) -> Result<Vec<u8>, String> {
    let exit: SpillExitMirror = serde_json::from_value(frame.clone())
        .map_err(|error| format!("spill exit frame is invalid: {error}"))?;
    let facts = state
        .pending_spill
        .lock()
        .expect("pending spill lock")
        .remove(&exit.token);
    let Some(facts) = facts else {
        return Err(format!(
            "spill conversation {} has no enter leg (state bug)",
            exit.token
        ));
    };
    let Some(decision) = exit.decision else {
        return Err("spill exit frame lacks decision".to_owned());
    };
    // The dsh gate: `decision.kind !== 'accept' || Object.hasOwn(decision,
    // 'value') || exec.name === 'read'` — the decision passes untouched.
    let untouched = |decision: &Value| -> Result<Vec<u8>, String> {
        serde_json::to_vec(&serde_json::json!({
            "kind": "final",
            "decision": decision,
        }))
        .map_err(|error| format!("spill reply encode failed: {error}"))
    };
    if decision.get("kind").and_then(Value::as_str) != Some("accept")
        || decision.get("value").is_some_and(|value| !value.is_null())
        || facts.name == "read"
    {
        return untouched(&decision);
    }
    let has_images = decision_content(&decision, &facts).iter().any(is_image);
    if facts.parent.is_some() && !has_images {
        // Text-only PTC bindings retain their asynchronous log-only
        // spill path.
        return untouched(&decision);
    }
    let content = decision_content(&decision, &facts);
    let retained = run_spill_bound(
        state,
        facts.agent.as_deref(),
        &content,
        &facts.name,
        &facts.call_id,
        if facts.parent.is_some() {
            "dispatch"
        } else {
            "result"
        },
    );
    let Some(retained) = retained else {
        return untouched(&decision);
    };
    // The PTC outer-result capture: an error-free image dispatch whose
    // replacement lost its images ferries the retained blocks as a
    // ptc-mode context message.
    let ptc_context =
        (facts.parent.is_some() && !facts.is_error && has_images && !retained.iter().any(is_image))
            .then(|| ptc_mode_message(&retained));
    let decision = compose_accept(&decision, &retained, ptc_context);
    serde_json::to_vec(&serde_json::json!({
        "kind": "final",
        "decision": decision,
    }))
    .map_err(|error| format!("spill reply encode failed: {error}"))
}

/// The `tools/ptc-dispatch-log` exit leg (the dsh dispatch-log listener
/// body): the bounded log copy replaces the settled content when the
/// bound reshapes it.
fn serve_spill_dispatch_log_exit(state: &ShellState, frame: &Value) -> Result<Vec<u8>, String> {
    let exit: SpillLogExitMirror = serde_json::from_value(frame.clone())
        .map_err(|error| format!("spill exit frame is invalid: {error}"))?;
    let facts = state
        .pending_spill
        .lock()
        .expect("pending spill lock")
        .remove(&exit.token);
    let Some(facts) = facts else {
        return Err(format!(
            "spill conversation {} has no enter leg (state bug)",
            exit.token
        ));
    };
    let retained = run_spill_bound(
        state,
        facts.agent.as_deref(),
        &exit.content,
        &facts.name,
        &facts.call_id,
        "dispatch",
    );
    let content = retained.unwrap_or(exit.content);
    serde_json::to_vec(&serde_json::json!({
        "kind": "final",
        "content": content,
    }))
    .map_err(|error| format!("spill reply encode failed: {error}"))
}

/// Runs the bound over the shell's seams, rendering the dsh catch-all
/// as the warn-plus-keep-inline outcome.
fn run_spill_bound(
    state: &ShellState,
    session_id: Option<&str>,
    content: &[fr_wire::ContentBlock],
    tool_name: &str,
    call_id: &str,
    label: &str,
) -> Option<Vec<fr_wire::ContentBlock>> {
    let max_tokens = state.config.spill_max_inline_tokens?;
    let seams = WireSpillSeams {
        link: &state.link,
        store: &state.spill,
    };
    match spill_bound(
        &seams, max_tokens, session_id, content, tool_name, call_id, label,
    ) {
        Ok(retained) => retained,
        Err(reason) => {
            spill_warn(&format!(
                "spill-policy: {reason}; keeping the inline content"
            ));
            None
        }
    }
}

/// The decision's content: a content replacement wins over the enter
/// leg's dispatch-stage content (the dsh
/// `decision.content ?? result.content`).
fn decision_content(decision: &Value, facts: &SpillEnterFacts) -> Vec<fr_wire::ContentBlock> {
    let replaced = decision
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .cloned()
                .map(|block| {
                    serde_json::from_value::<fr_wire::ContentBlock>(block)
                        .unwrap_or_else(|_| crate::spill_policy::text_block(""))
                })
                .collect::<Vec<_>>()
        });
    let Some(replaced) = replaced else {
        return facts.content.clone();
    };
    replaced
}

/// Composes the accept decision over the replacement (the dsh
/// `{kind:'accept', content: retained, ...additionalContexts}` spread).
fn compose_accept(
    decision: &Value,
    retained: &[fr_wire::ContentBlock],
    ptc_context: Option<Value>,
) -> Value {
    let mut composed = serde_json::json!({
        "kind": "accept",
        "content": retained,
    });
    let mut contexts = decision
        .get("additionalContexts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(context) = ptc_context {
        contexts.push(context);
    }
    if !contexts.is_empty() {
        composed["additionalContexts"] = Value::Array(contexts);
    }
    composed
}

/// Builds the ptc-mode context message (the dsh
/// `createUserMessage({content, source: {kind: 'ptc-mode'}})`; the
/// message id mints a fresh uuid v4 like every producer-side id).
fn ptc_mode_message(content: &[fr_wire::ContentBlock]) -> Value {
    serde_json::json!({
        "role": "user",
        "id": uuid::Uuid::new_v4().to_string(),
        "content": content,
        "source": { "kind": "ptc-mode" },
    })
}

/// The consumer-side mirror of the spill enter frame (the fr-tools
/// `PostEnter` wire shape; only the fields the listener composes
/// against ride the mirror).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpillEnterMirror {
    token: u64,
    call: SpillCallMirror,
    result: SpillResultMirror,
}

/// The execution view the spill listener reads (the dsh `exec`:
/// owner, tool name, call identity, and the PTC parent token).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpillCallMirror {
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    parent: Option<String>,
}

/// The result view the spill listener reads (the dsh `result`:
/// model-facing content and the failure flag).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpillResultMirror {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    content: Vec<fr_wire::ContentBlock>,
}

impl SpillResultMirror {
    /// Whether the dispatch-stage result failed.
    fn is_error(&self) -> bool {
        self.kind == "failure"
    }

    /// The model-facing content of either outcome.
    fn content(&self) -> Vec<fr_wire::ContentBlock> {
        self.content.clone()
    }
}

/// The consumer-side mirror of the post-execute exit frame (the
/// fr-tools `PostExit` shape; the decision stays a raw wire value the
/// gate reads field-wise).
#[derive(Deserialize)]
struct SpillExitMirror {
    token: u64,
    #[serde(default)]
    decision: Option<Value>,
}

/// The consumer-side mirror of the dispatch-log exit frame (the
/// fr-tools `LogExit` shape).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpillLogExitMirror {
    token: u64,
    #[serde(default)]
    content: Vec<fr_wire::ContentBlock>,
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
    let prune = PruneHook {
        session_id: session_id.to_owned(),
        link: state.link,
        config: state.config.prune,
    };
    let capacity = CapacityHook { link: state.link };
    let mut deps = EngineDeps {
        meter: &meter,
        summarize: &mut |input| summarize.run(input),
        recover: &mut |failure, seqs, view| recover.run(failure, seqs, view),
        model_capacity: &mut |provider, model| capacity.run(provider, model),
        prune: Some(&mut |view| prune.run(view)),
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

/// Runs the request-error image-offload arm: the durable surface
/// repair (the dsh listener's offload-and-retry ruling).
///
/// The dsh `signal.aborted` short-circuit has no wire face: the loop
/// driver polls its latch before dispatching, and a cancel landing
/// mid-dispatch cannot reach a foreign entry (the 17f documented
/// deviation; the auto path only).
fn run_offload_arm(state: &ShellState, session_id: &str, count: u64) -> Result<bool, String> {
    let mut view = state.open_session(session_id)?;
    let surface = view.surface_nodes().to_vec();
    crate::image_offload::offload_oldest_images(&mut view, &surface, count)
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
    // The image-offload arm first (the dsh request-error listener pair;
    // the codes are disjoint so the shared entry dispatches by code).
    if event.failure.code.as_deref() == Some(IMAGE_OFFLOAD_REQUIRED_CODE)
        && let Some(count) = event.failure.offload_images
    {
        return run_offload_arm(state, &event.session_id, count);
    }
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
    let prune = PruneHook {
        session_id: event.session_id.clone(),
        link: state.link,
        config: state.config.prune,
    };
    let capacity = CapacityHook { link: state.link };
    let mut deps = EngineDeps {
        meter: &meter,
        summarize: &mut |input| summarize.run(input),
        recover: &mut |failure, seqs, view| recover.run(failure, seqs, view),
        model_capacity: &mut |provider, model| capacity.run(provider, model),
        prune: Some(&mut |view| prune.run(view)),
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
    fn run(
        &self,
        failure: &CompactionFailure,
        seqs: &[fr_wire::SessionSeq],
        view: &mut dyn crate::region::SessionView,
    ) -> bool {
        // The in-process image-offload consumer (the dsh summary-error
        // listener): the built-in answers before any external dispatch
        // (the dsh waterfall's registration order), and a kernel
        // round-trip to self mid-entry is deterministically refused by
        // the per-instance lock (the D-V3-3 wait graph), so the
        // built-in arm runs here against the transaction's own view.
        if let CompactionFailure::Llm {
            code: Some(code),
            failure: Some(provider_failure),
            ..
        } = failure
            && code == IMAGE_OFFLOAD_REQUIRED_CODE
            && let Some(count) = provider_failure.offload_images
        {
            match crate::image_offload::offload_oldest_images(view, seqs, count) {
                Ok(true) => return true,
                Ok(false) => {}
                Err(error) => {
                    log_warn_global(&format!("compaction summary-error offload failed: {error}"));
                    return false;
                }
            }
        }
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

/// The prune hook (the dsh `ctx.get('toolResultPruner')` face): the
/// one-crate pruner over the wire-backed view and meter. The dsh
/// soft-get's absent arm stays expressible at the engine seam; the
/// one-crate assembly always provides it.
struct PruneHook {
    session_id: String,
    link: HostLink,
    config: crate::pruner::PruneConfig,
}

impl PruneHook {
    fn run(
        &self,
        view: &mut dyn crate::region::SessionView,
    ) -> Result<crate::pruner::PruneResult, String> {
        let meter = RemoteMeter {
            link: self.link,
            session_id: self.session_id.clone(),
        };
        crate::pruner::prune_session(view, &meter, &self.config)
    }
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

/// The spill warning sink (the dsh `ctx.logger.warn` in the spill
/// catch-all template).
fn spill_warn(message: &str) {
    eprintln!("{message}");
}

/// Runs the one-shot startup sweep (the dsh `runCleanup`): discover the
/// prior default roots plus the active root, sweep at the age cutoff.
/// Best-effort — every filesystem failure lands in the warn sink.
fn run_startup_sweep(root: &std::path::Path, days: u64) {
    const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;
    let cutoff = now_ms().saturating_sub(days.saturating_mul(MS_PER_DAY));
    let warn = |message: &str| log_warn_global(message);
    let roots = gather_sweep_roots(root, &std::env::temp_dir(), &warn);
    sweep_spill_roots(&roots, signed_cutoff(cutoff), &warn);
}

/// The JS-Number-parity millisecond clock (the dsh `Date.now()`).
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "millisecond wall clocks fit the u64 window exactly"
)]
fn now_ms() -> u64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    elapsed.as_millis() as u64
}

/// The sweep cutoff at the signed width the cleanup walk prices.
#[must_use]
#[expect(
    clippy::cast_possible_wrap,
    reason = "the millisecond cutoff stays far below i64::MAX"
)]
const fn signed_cutoff(cutoff: u64) -> i64 {
    cutoff as i64
}

/// The spill-policy wire seams: the require clients the endpoints read
/// (the dsh `ctx` faces; the attachments and fs reads stay soft — an
/// undeclared or uncommitted provider is the absent arm). The local
/// backend rides the struct (the one-crate fold).
struct WireSpillSeams<'a> {
    link: &'a HostLink,
    store: &'a LocalSpillStore,
}

impl SpillSeams for WireSpillSeams<'_> {
    fn estimate(&self, blocks: &[fr_wire::ContentBlock]) -> Result<u64, String> {
        let payload = serde_json::json!({ "blocks": blocks });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("estimate encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::token_meter::SERVICE_NAME,
            fr_wire::services::token_meter::METHOD_ESTIMATE_CONTENT,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("estimate decode failed: {error}"))?;
        parsed
            .get("tokens")
            .and_then(Value::as_u64)
            .ok_or_else(|| "estimate reply lacks tokens".to_owned())
    }

    fn price_images(
        &self,
        provider: &str,
        model: &str,
        images: &[fr_wire::ContentBlock],
    ) -> Result<Vec<ImagePrice>, String> {
        let payload = serde_json::json!({ "provider": provider, "model": model, "images": images });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("pricing encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::llm::SERVICE_NAME,
            fr_wire::services::llm::METHOD_IMAGE_REQUEST_PRICING,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("pricing decode failed: {error}"))?;
        let prices = parsed
            .get("prices")
            .ok_or_else(|| "pricing reply lacks prices".to_owned())?;
        if prices.is_null() {
            // A route without declared pricing keeps the estimator (the
            // dsh soft-get `undefined` folds into the same refusal as a
            // missing route).
            return Err(crate::spill_policy::no_image_calculator());
        }
        let prices = prices
            .as_array()
            .ok_or_else(|| "pricing reply is not a list".to_owned())?;
        prices
            .iter()
            .map(|price| {
                Ok(ImagePrice {
                    visual_tokens: price
                        .get("visualTokens")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| "price lacks visualTokens".to_owned())?,
                    text: price
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "price lacks text".to_owned())?
                        .to_owned(),
                })
            })
            .collect()
    }

    fn image_access(&self, reference: &fr_wire::ImageAttachmentRef) -> Option<String> {
        let payload = serde_json::json!({ "ref": reference });
        let bytes = serde_json::to_vec(&payload).ok()?;
        // The attachments and fs reads stay soft: a refused require is
        // the absent provider (the dsh `ctx.get` undefined pair).
        let reply = self
            .link
            .require(
                fr_wire::services::attachments::SERVICE_NAME,
                fr_wire::services::attachments::METHOD_IMAGE_HOST_PATH,
                &bytes,
            )
            .ok()?;
        let parsed: Value = serde_json::from_slice(&reply).ok()?;
        let host_path = parsed.get("path")?.as_str()?.to_owned();
        let map = serde_json::json!({ "hostPath": host_path });
        let bytes = serde_json::to_vec(&map).ok()?;
        let reply = self
            .link
            .require(
                fr_wire::services::fs::SERVICE_NAME,
                fr_wire::services::fs::METHOD_PROCESS_PATH_FROM_HOST_PATH,
                &bytes,
            )
            .ok()?;
        let parsed: Value = serde_json::from_slice(&reply).ok()?;
        parsed.get("path")?.as_str().map(str::to_owned)
    }

    fn request_header_route(&self, session_id: &str) -> Result<Option<(String, String)>, String> {
        let payload = serde_json::json!({ "id": session_id });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("request-header encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_REQUEST_HEADER,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("request-header decode failed: {error}"))?;
        // The `{header: {config}, reason}` envelope (the session-view
        // precedent); the config carries the routed provider/model.
        let config = parsed.get("header").and_then(|header| header.get("config"));
        Ok(route_of(config))
    }

    fn agent_route(&self, session_id: &str) -> Result<Option<(String, String)>, String> {
        let payload = serde_json::json!({ "sessionId": session_id });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("agent status encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::agent::SERVICE_NAME,
            fr_wire::services::agent::METHOD_STATUS,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("agent status decode failed: {error}"))?;
        Ok(route_of(Some(&parsed)))
    }

    fn save_text(&self, save: &SaveTextSpill) -> Result<crate::spill::SpillRef, String> {
        // The one-crate fold: the spill endpoints read the local backend
        // in-process (a self-instance require is the deterministic
        // reentrancy refusal).
        crate::spill::SpillStore::save_text(self.store, save)
    }
}

/// Extracts the provider/model pair from one wire route object.
fn route_of(value: Option<&Value>) -> Option<(String, String)> {
    let value = value?;
    let provider = value.get("provider")?.as_str()?.to_owned();
    let model = value.get("model")?.as_str()?.to_owned();
    Some((provider, model))
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
    state.join_threads();
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
    use std::collections::HashMap;
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
        /// Every `'llm'` BEGIN request body, in call order (the
        /// summarizer-input pin point).
        begin_log: Mutex<Vec<serde_json::Value>>,
        /// The grants the shell issued, in call order.
        grants: Mutex<Vec<(String, String)>>,
        /// Every `'tools'` registration body, in call order.
        tools_calls: Mutex<Vec<serde_json::Value>>,
        /// Scripted estimate-content prices (unscripted texts price at
        /// the fixed density the real estimator pins).
        estimates: Mutex<HashMap<String, u64>>,
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
    fn serve_llm(
        state: &TestHost,
        method: u32,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match method {
            fr_wire::services::llm::METHOD_RESOLVE_MODEL_INFO => Ok(serde_json::json!({
                "provider": "deepseek", "id": "m1", "name": "M1",
                "context": { "contextWindow": 262_144 },
                "defaultMaxTokens": 4_096,
            })),
            fr_wire::services::llm::METHOD_BEGIN => {
                state
                    .begin_log
                    .lock()
                    .expect("host begin lock")
                    .push(payload.clone());
                Ok(serde_json::json!({ "generationId": "g1" }))
            }
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
            fr_wire::services::llm::METHOD_IMAGE_REQUEST_PRICING => Ok(
                serde_json::json!({ "prices": [{ "visualTokens": 900, "text": "image handle text" }] }),
            ),
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
                fr_wire::services::token_meter::METHOD_ESTIMATE_CONTENT => {
                    let text = payload
                        .get("blocks")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|blocks| blocks.first())
                        .and_then(|block| block.get("text"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let scripted = state
                        .estimates
                        .lock()
                        .expect("host estimates lock")
                        .get(text)
                        .copied()
                        .unwrap_or_else(|| (text.chars().count() as u64).div_ceil(4) + 4);
                    Ok(serde_json::json!({ "tokens": scripted }))
                }
                _ => Err(format!("unknown token-meter method {method}")),
            },
            fr_wire::services::tools::SERVICE_NAME => {
                state
                    .tools_calls
                    .lock()
                    .expect("host tools lock")
                    .push(payload);
                Ok(serde_json::json!({ "id": 1 }))
            }
            fr_wire::services::llm::SERVICE_NAME => serve_llm(state, method, &payload),
            fr_wire::services::agent::SERVICE_NAME => {
                if state.agent_fails.load(Ordering::Acquire) {
                    return STATUS_FAILED;
                }
                Ok(serve_agent(state))
            }
            fr_wire::services::attachments::SERVICE_NAME => Ok(serde_json::json!({
                "path": "/host/shot.png",
            })),
            fr_wire::services::fs::SERVICE_NAME => Ok(serde_json::json!({
                "path": "/exec/shot.png",
            })),
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
        host: *mut c_void,
        target: *const u8,
        target_len: usize,
        granted: *const u8,
        granted_len: usize,
    ) -> i32 {
        // SAFETY: `host` is the `TestHost` the harness installed.
        let state = unsafe { &*host.cast::<TestHost>() };
        // SAFETY: name buffers valid for their lengths per the contract.
        let target =
            unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(target, target_len)) };
        // SAFETY: name buffer valid for its length per the contract.
        let granted =
            unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(granted, granted_len)) };
        state
            .grants
            .lock()
            .expect("host grants lock")
            .push((target.into_owned(), granted.into_owned()));
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
                "image/offload".to_owned(),
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
            begin_log: Mutex::new(Vec::new()),
            grants: Mutex::new(Vec::new()),
            tools_calls: Mutex::new(Vec::new()),
            estimates: Mutex::new(std::collections::HashMap::new()),
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

    /// Builds one user/message event body carrying `count` image
    /// attachments (the decision writer's target shape).
    fn image_user_body(id: &str, count: usize) -> serde_json::Value {
        serde_json::json!({
            "role": "user",
            "id": id,
            "content": (0..count)
                .map(|index| serde_json::json!({
                    "type": "image",
                    "attachment": {
                        "attachmentId": format!("a-{id}-{index}"),
                        "mediaType": "image/png",
                        "bytes": 10,
                        "width": 1,
                        "height": 1,
                    },
                }))
                .collect::<Vec<_>>(),
            "source": { "kind": "user" },
        })
    }

    /// Appends one plain text user message to the live session.
    fn seed_plain_user(host: &TestHost) {
        let mut sessions = host.sessions.lock().expect("host session lock");
        session_dispatch(
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            serde_json::json!({
                "id": "s1", "type": "user/message",
                "data": {
                    "role": "user",
                    "id": "usr-trailer",
                    "content": [{ "type": "text", "text": "after" }],
                    "source": { "kind": "user" },
                },
                "surfaceOp": "append",
            })
            .to_string()
            .as_bytes(),
            &mut sessions,
        )
        .expect("seed plain user");
    }

    /// Appends one image-carrying user message to the live session.
    fn seed_image_user(host: &TestHost, id: &str, count: usize) {
        let mut sessions = host.sessions.lock().expect("host session lock");
        session_dispatch(
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            serde_json::json!({
                "id": "s1", "type": "user/message", "data": image_user_body(id, count),
                "surfaceOp": "append",
            })
            .to_string()
            .as_bytes(),
            &mut sessions,
        )
        .expect("seed image user");
    }

    /// The failing summary batch: a terminal error finish carrying the
    /// `IMAGE_OFFLOAD_REQUIRED` failure facts.
    fn offload_fail_batch(count: u64) -> serde_json::Value {
        serde_json::json!({
            "generationId": "g1",
            "chunks": [
                { "type": "finish", "reason": { "kind": "error", "failure": {
                    "message": "offload", "code": "IMAGE_OFFLOAD_REQUIRED",
                    "offloadImages": count,
                } } },
            ],
            "done": true,
        })
    }

    /// The overflow recovery rides the pruner and the summary-error
    /// offload arm: the failing summary reports one offload, the
    /// built-in consumer lands the decision in the transaction view,
    /// the re-prepared input carries the offloaded mark, and the retry
    /// completes the compaction into a ruled retry.
    #[test]
    fn overflow_offload_recovery_completes_with_marks() {
        let (mut state, host) = harness(
            "offload-recover",
            vec![offload_fail_batch(1), checkpoint_batch("recovered body")],
            true,
        );
        seed_image_user(&host, "u-img", 1);
        // A trailing plain node keeps the image inside the shadowed
        // region (the selection retains the newest surface node).
        seed_plain_user(&host);

        let failure =
            br#"{"sessionId":"s1","failure":{"message":"window","code":"CONTEXT_WINDOW_EXCEEDED"}}"#;
        let (status, reply) = drive(&mut state, EV_REQUEST_ERROR, failure);
        assert_eq!(status, STATUS_OK);
        assert_eq!(reply, RETRY_CHAIN_VALUE, "the recovery rules a retry");

        // The durable decision landed before the compaction close.
        let types = event_types(&host);
        assert!(
            types.contains(&"image/offload".to_owned()),
            "the decision event lands: {types:?}"
        );
        assert!(types.contains(&"compaction/end".to_owned()), "{types:?}");

        // The re-prepared summarizer input carries the projected mark.
        let begins = host.begin_log.lock().expect("begin log").clone();
        assert!(begins.len() >= 2, "two summarizer calls: {}", begins.len());
        let second = &begins[1];
        let messages = second["request"]["messages"].as_array().expect("messages");
        let offloaded = serde_json::to_string(&messages).expect("json");
        assert!(
            offloaded.contains("\"offloaded\":true"),
            "the re-prepared input sees the offloaded mark: {offloaded}"
        );
        state.join_relay();
    }

    /// The request-error image-offload arm: the offload count selects
    /// the oldest un-offloaded occurrences in surface order and rules
    /// the retry without spending overflow budget.
    #[test]
    fn request_error_offload_arm_rules_retry() {
        let (mut state, host) = harness("offload-req", vec![], true);
        seed_image_user(&host, "u-a", 1);
        seed_image_user(&host, "u-b", 2);

        let failure = br#"{"sessionId":"s1","failure":{"message":"budget","code":"IMAGE_OFFLOAD_REQUIRED","offloadImages":2}}"#;
        let (status, reply) = drive(&mut state, EV_REQUEST_ERROR, failure);
        assert_eq!(status, STATUS_OK);
        assert_eq!(reply, RETRY_CHAIN_VALUE, "the landed offload rules a retry");

        let types = event_types(&host);
        assert!(
            types.iter().filter(|t| *t == "image/offload").count() == 1,
            "one decision event: {types:?}"
        );
        // The decision selects the oldest two occurrences across the
        // two messages in surface order.
        let sessions = host.sessions.lock().expect("host session lock");
        let session = sessions
            .store()
            .get(&fr_wire::ids::SessionId::new("s1"))
            .expect("live session");
        let decision = session
            .events()
            .iter()
            .rev()
            .find(|event| event.event_type() == "image/offload")
            .expect("decision event");
        let fr_wire::event::SessionEventData::Unknown(unknown) = &decision.data else {
            panic!("unknown carrier");
        };
        let data = serde_json::Value::Object(unknown.data.clone());
        let targets = data["targets"].as_array().expect("targets");
        assert_eq!(targets.len(), 2);
        // The harness seeds four plain users first, so the image
        // messages occupy surface nodes 6 and 7; the oldest two
        // occurrences spread across both in surface order.
        assert_eq!(targets[0]["seq"], serde_json::json!(6));
        assert_eq!(targets[0]["imageIndexes"], serde_json::json!([0]));
        assert_eq!(targets[1]["seq"], serde_json::json!(7));
        assert_eq!(targets[1]["imageIndexes"], serde_json::json!([0]));
        drop(sessions);
        state.join_relay();
    }

    /// Nothing to offload keeps the chain untouched (the dsh `next()`
    /// arm): no decision event and the identity echo.
    #[test]
    fn request_error_offload_arm_defers_without_targets() {
        let (mut state, _host) = harness("offload-defer", vec![], true);
        let failure = br#"{"sessionId":"s1","failure":{"message":"budget","code":"IMAGE_OFFLOAD_REQUIRED","offloadImages":1}}"#;
        let (status, reply) = drive(&mut state, EV_REQUEST_ERROR, failure);
        assert_eq!(status, STATUS_OK);
        assert_eq!(reply, failure, "the untouched chain echoes the payload");
        state.join_relay();
    }

    /// The pruner lands over-budget tool results as shadow-priced
    /// replacements inside the overflow recovery, and the pass runs
    /// before the range selection (the dsh overflow arm).
    #[test]
    fn overflow_prune_lands_shadow_priced_replacements() {
        let (mut state, host) = harness(
            "prune-recover",
            vec![checkpoint_batch("checkpoint body")],
            true,
        );
        // A tool result whose text far exceeds the 8192 default.
        let long_text = "x".repeat(9_000);
        let mut sessions = host.sessions.lock().expect("host session lock");
        session_dispatch(
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            serde_json::json!({
                "id": "s1", "type": "tool/result",
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "tool", "id": "t-1",
                        "content": [{ "type": "text", "text": long_text }],
                        "source": { "kind": "tool", "callId": "c-1" },
                        "toolCallId": "c-1",
                    },
                },
                "surfaceOp": "append",
            })
            .to_string()
            .as_bytes(),
            &mut sessions,
        )
        .expect("seed tool result");
        drop(sessions);
        // A trailing plain node keeps the tool result inside the shadowed
        // region (the selection retains the newest surface node).
        {
            let mut sessions = host.sessions.lock().expect("host session lock");
            session_dispatch(
                fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
                serde_json::json!({
                    "id": "s1", "type": "user/message",
                    "data": {
                        "role": "user",
                        "id": "usr-trailer",
                        "content": [{ "type": "text", "text": "after" }],
                        "source": { "kind": "user" },
                    },
                    "surfaceOp": "append",
                })
                .to_string()
                .as_bytes(),
                &mut sessions,
            )
            .expect("seed trailer");
        }

        let failure =
            br#"{"sessionId":"s1","failure":{"message":"window","code":"CONTEXT_WINDOW_EXCEEDED"}}"#;
        let (status, reply) = drive(&mut state, EV_REQUEST_ERROR, failure);
        assert_eq!(status, STATUS_OK);
        assert_eq!(reply, RETRY_CHAIN_VALUE, "the prune progress rules a retry");

        let types = event_types(&host);
        assert!(
            types.contains(&"compaction/prune".to_owned()),
            "the shadow price lands: {types:?}"
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

    /// The spill harness: the standard seed plus the cap, the temp
    /// spill root, and a disabled sweep (the sweep runs on its own
    /// tests).
    fn spill_harness(name: &str) -> (Box<ShellState>, Arc<TestHost>, std::path::PathBuf) {
        let root = temp_root(name);
        let gate = serde_json::json!({
                "auto": false,
                // The cap sits between the reserved notice (the temp
                // locator lengthens it) and the 104-token content.
                "max-inline-tokens": 90,
                "spill-root": root.display().to_string(),
                "cleanup-period-days": 0,
        });
        let host_pairs = harness_with_config(name, Vec::new(), &gate);
        (host_pairs.0, host_pairs.1, root)
    }

    /// Builds the harness over an explicit config gate.
    fn harness_with_config(
        name: &str,
        llm_batches: Vec<serde_json::Value>,
        gate: &serde_json::Value,
    ) -> (Box<ShellState>, Arc<TestHost>) {
        let mut vocabulary = EventVocabulary::core();
        vocabulary
            .declare(&[
                "compaction/start".to_owned(),
                "compaction/summary".to_owned(),
                "compaction/end".to_owned(),
                "compaction/prune".to_owned(),
                "image/offload".to_owned(),
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
        session_dispatch(
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            br#"{"id":"s1","type":"request/header","data":{"header":{"config":{"provider":"deepseek","model":"m1","maxTokens":1024}},"reason":"initial"}}"#,
            &mut sessions,
        )
        .expect("seed header");
        let host = Arc::new(TestHost {
            sessions: Mutex::new(sessions),
            llm_batches: Mutex::new(llm_batches),
            agent_route: Mutex::new(Some((Some("deepseek".to_owned()), Some("m1".to_owned())))),
            agent_fails: AtomicBool::new(false),
            dispatch_log: Mutex::new(Vec::new()),
            begin_log: Mutex::new(Vec::new()),
            grants: Mutex::new(Vec::new()),
            tools_calls: Mutex::new(Vec::new()),
            estimates: Mutex::new(std::collections::HashMap::new()),
        });
        let exports = HostExports {
            grant: host_grant,
            dispatch: host_dispatch,
            host_handle: Arc::into_raw(Arc::clone(&host)).cast::<c_void>().cast_mut(),
            require: host_require,
            emit: host_emit,
        };
        let config = resolve_config(gate).expect("harness config resolves");
        let state = Box::new(ShellState::assembled(super::HostLink::new(exports), config));
        (state, host)
    }

    /// The oversized plain-text dispatch-stage result the spill arms
    /// consume.
    fn oversized_result() -> serde_json::Value {
        serde_json::json!({
            "kind": "success",
            "content": [{ "type": "text", "text": "a".repeat(400) }],
            "additionalContexts": [],
        })
    }

    /// Serves the two spill legs through the public dispatch face.
    fn spill_frame(
        state: &mut ShellState,
        method: u32,
        frame: &serde_json::Value,
    ) -> serde_json::Value {
        let reply =
            super::dispatch(method, frame.to_string().as_bytes(), state).expect("spill frame");
        serde_json::from_slice(&reply).expect("spill reply")
    }

    /// The post-execute arm bounds an over-cap accepted result: the
    /// decision composes the bounded copy and the artifact lands in the
    /// configured root.
    #[test]
    fn spill_post_execute_bounds_the_accepted_result() {
        let (mut state, _host, root) = spill_harness("spill-post");
        let enter = serde_json::json!({
            "stage": "post-execute",
            "leg": "enter",
            "token": 5,
            "call": { "callId": "c1", "name": "huge_read", "agent": "s1" },
            "result": oversized_result(),
        });
        let reply = spill_frame(
            &mut state,
            fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE,
            &enter,
        );
        assert_eq!(reply["kind"], "delegate", "the listener calls next first");

        let exit = serde_json::json!({
            "stage": "post-execute",
            "leg": "exit",
            "token": 5,
            "decision": { "kind": "accept", "additionalContexts": [] },
        });
        let reply = spill_frame(
            &mut state,
            fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE,
            &exit,
        );
        assert_eq!(reply["kind"], "final");
        let decision = &reply["decision"];
        assert_eq!(decision["kind"], "accept");
        let content = decision["content"].as_array().expect("content");
        let inline = content
            .iter()
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("");
        assert!(inline.contains(crate::spill_policy::GAP_TEXT), "{inline}");
        assert!(
            inline.contains("Full formatted result stored at: "),
            "the locator rides the footer: {inline}"
        );
        // The artifact landed under the configured root with the full
        // text and the dsh retrieval hint.
        let session_dirs: Vec<_> = std::fs::read_dir(&root)
            .expect("root reads")
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(session_dirs.len(), 1, "one session directory");
    }

    /// The post-execute gates pass the untouched decision through: the
    /// read tool, value replacements, blocks, and text-only PTC parents.
    #[test]
    fn spill_post_execute_gates_pass_through() {
        let (mut state, _host, _root) = spill_harness("spill-gates");
        let run = |state: &mut ShellState,
                   token: u64,
                   call: serde_json::Value,
                   decision: serde_json::Value| {
            let enter = serde_json::json!({
                "stage": "post-execute",
                "leg": "enter",
                "token": token,
                "call": call,
                "result": oversized_result(),
            });
            let reply = spill_frame(
                state,
                fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE,
                &enter,
            );
            assert_eq!(reply["kind"], "delegate");
            spill_frame(
                state,
                fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE,
                &serde_json::json!({
                    "stage": "post-execute",
                    "leg": "exit",
                    "token": token,
                    "decision": decision,
                }),
            )
        };
        // The read tool never spills (the read->spill->read loop).
        let reply = run(
            &mut state,
            1,
            serde_json::json!({ "callId": "c1", "name": "read", "agent": "s1" }),
            serde_json::json!({ "kind": "accept", "additionalContexts": [] }),
        );
        assert_eq!(
            reply["decision"],
            serde_json::json!({ "kind": "accept", "additionalContexts": [] })
        );
        // A block decision passes untouched.
        let reply = run(
            &mut state,
            2,
            serde_json::json!({ "callId": "c1", "name": "huge_read", "agent": "s1" }),
            serde_json::json!({ "kind": "block", "feedback": [{ "type": "text", "text": "no" }], "additionalContexts": [] }),
        );
        assert_eq!(reply["decision"]["kind"], "block");
        // A value replacement passes untouched.
        let reply = run(
            &mut state,
            3,
            serde_json::json!({ "callId": "c1", "name": "huge_read", "agent": "s1" }),
            serde_json::json!({ "kind": "accept", "value": "replaced", "additionalContexts": [] }),
        );
        assert_eq!(reply["decision"]["value"], "replaced");
        // A text-only PTC parent rides the dispatch-log path.
        let reply = run(
            &mut state,
            4,
            serde_json::json!({ "callId": "c1", "name": "huge_read", "agent": "s1", "parent": "t1" }),
            serde_json::json!({ "kind": "accept", "additionalContexts": [] }),
        );
        assert_eq!(reply["decision"]["content"], serde_json::Value::Null);
    }

    /// The dispatch-log arm bounds only the logged copy: text bounds,
    /// non-text declines, and within-cap logs stay untouched.
    #[test]
    fn spill_dispatch_log_bounds_only_the_log_copy() {
        let (mut state, _host, _root) = spill_harness("spill-log");
        let enter = serde_json::json!({
            "stage": "dispatch-log",
            "leg": "enter",
            "token": 7,
            "call": { "callId": "parent-1:ptc:1", "name": "huge_read", "agent": "s1" },
            "result": oversized_result(),
        });
        let reply = spill_frame(
            &mut state,
            fr_wire::services::compaction::METHOD_SPILL_DISPATCH_LOG,
            &enter,
        );
        assert_eq!(reply["kind"], "delegate");
        let exit = serde_json::json!({
            "stage": "dispatch-log",
            "leg": "exit",
            "token": 7,
            "content": [{ "type": "text", "text": "a".repeat(400) }],
        });
        let reply = spill_frame(
            &mut state,
            fr_wire::services::compaction::METHOD_SPILL_DISPATCH_LOG,
            &exit,
        );
        assert_eq!(reply["kind"], "final");
        let logged = reply["content"].as_array().expect("content");
        let text = logged
            .iter()
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains("Full formatted result stored at: "),
            "the logged copy carries the locator: {text}"
        );
        assert!(text.chars().count() < 400, "the copy is bounded: {text}");

        // Non-text content declines and rides through verbatim.
        let enter = serde_json::json!({
            "stage": "dispatch-log",
            "leg": "enter",
            "token": 8,
            "call": { "callId": "parent-1:ptc:2", "name": "mixed", "agent": "s1" },
            "result": { "kind": "success", "content": [{ "type": "reasoning", "text": "why" }] },
        });
        let reply = spill_frame(
            &mut state,
            fr_wire::services::compaction::METHOD_SPILL_DISPATCH_LOG,
            &enter,
        );
        assert_eq!(reply["kind"], "delegate");
        let exit = serde_json::json!({
            "stage": "dispatch-log",
            "leg": "exit",
            "token": 8,
            "content": [{ "type": "reasoning", "text": "why" }],
        });
        let reply = spill_frame(
            &mut state,
            fr_wire::services::compaction::METHOD_SPILL_DISPATCH_LOG,
            &exit,
        );
        assert_eq!(
            reply["content"],
            serde_json::json!([{ "type": "reasoning", "text": "why" }]),
            "the flatten declines"
        );
    }

    /// The spill-store face persists through the local backend and
    /// answers the dsh `SpillRef` wire shape.
    #[test]
    fn spill_store_face_saves_and_answers_the_reference() {
        let (mut state, _host, root) = spill_harness("spill-store-face");
        let reply = super::dispatch(
            fr_wire::services::spill_store::METHOD_SAVE_TEXT,
            br#"{"owner":{"sessionId":"s1"},"source":{"kind":"tool","toolName":"grep","callId":"c9","label":"result"},"suggestedName":"grep.txt","content":"hello spill"}"#,
            &mut state,
        )
        .expect("saves");
        let reference: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reference["bytes"], 11);
        assert_eq!(
            reference["retrievalHint"],
            "Use read with offset/limit, or grep this path to search within it."
        );
        let locator = reference["locator"].as_str().expect("locator");
        assert!(
            locator.starts_with(root.display().to_string().as_str()),
            "{locator}"
        );
        let saved = std::fs::read_to_string(locator).expect("the artifact reads back");
        assert_eq!(saved, "hello spill");
    }

    /// The endpoint registration: the grant plus the two tools calls in
    /// the dsh order, gated on the configured cap.
    #[test]
    fn spill_endpoints_register_into_the_tools_face() {
        let (mut state, host, _root) = spill_harness("spill-register");
        state.register_spill_endpoints().expect("registers");
        let grants = host.grants.lock().expect("grants").clone();
        assert_eq!(grants, vec![("tools".to_owned(), "compaction".to_owned())]);
        let calls = host.tools_calls.lock().expect("tools").clone();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["stage"], "post-execute");
        assert_eq!(calls[0]["endpoint"]["service"], "compaction");
        assert_eq!(
            calls[0]["endpoint"]["method"],
            fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE
        );
        assert_eq!(calls[1]["stage"], "dispatch-log");
        assert_eq!(
            calls[1]["endpoint"]["method"],
            fr_wire::services::compaction::METHOD_SPILL_DISPATCH_LOG
        );

        // The policy omitted: registration is a no-op (the dsh apply's
        // early return).
        let inert_gate = serde_json::json!({ "auto": false, "cleanup-period-days": 0 });
        let (mut inert, inert_host) = harness_with_config("spill-inert", Vec::new(), &inert_gate);
        inert.register_spill_endpoints().expect("no-op");
        assert!(inert_host.tools_calls.lock().expect("tools").is_empty());
        assert!(inert_host.grants.lock().expect("grants").is_empty());
    }

    /// The shipped descriptor pins the two provided services, the five
    /// injected faces, and the four listened events.
    #[test]
    fn descriptor_pins_provide_inject_and_listen() {
        let parsed: serde_json::Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], super::PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            parsed["provide"],
            serde_json::json!(["compaction", "spill-store"])
        );
        assert_eq!(
            parsed["inject"],
            serde_json::json!([
                "llm",
                "token-meter",
                "session-persistence",
                "agent",
                "tools"
            ])
        );
        assert_eq!(
            parsed["listen"],
            serde_json::json!({
                "agent/pre-step": 1,
                "agent/status": 2,
                "session/event": 3,
                "agent/request-error": 4,
            })
        );
    }

    /// The startup sweep thread joins at teardown and the disabled
    /// sweep spawns nothing (the join discipline).
    #[test]
    fn the_startup_sweep_joins_at_teardown() {
        let root = temp_root("spill-sweep");
        let sweep_gate = serde_json::json!({
            "auto": false,
            "spill-root": root.display().to_string(),
            "cleanup-period-days": 1,
        });
        let (mut state, _host) = harness_with_config("spill-sweep", Vec::new(), &sweep_gate);
        state.join_threads();
        // A second join is a no-op (the handle is taken).
        state.join_threads();
        // The disabled sweep: nothing to join, the call still converges.
        let off_gate = serde_json::json!({ "auto": false, "cleanup-period-days": 0 });
        let (mut inert, _host) = harness_with_config("spill-sweep-off", Vec::new(), &off_gate);
        inert.join_threads();
    }

    /// The full multimodal path rides the wire: the routed header picks
    /// the pricing face, the access pair renders the readonly path into
    /// the artifact, and the footer counts the omitted image.
    #[test]
    fn spill_images_price_and_render_through_the_wire() {
        let (mut state, _host, root) = spill_harness("spill-image-wire");
        let image = serde_json::json!({
            "type": "image",
            "attachment": {
                "attachmentId": "sha256:shot",
                "mediaType": "image/png",
                "bytes": 8,
                "width": 4,
                "height": 3,
            },
        });
        let enter = serde_json::json!({
            "stage": "post-execute",
            "leg": "enter",
            "token": 9,
            "call": { "callId": "c-img", "name": "shot", "agent": "s1" },
            "result": { "kind": "success", "content": [image], "additionalContexts": [] },
        });
        let reply = spill_frame(
            &mut state,
            fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE,
            &enter,
        );
        assert_eq!(reply["kind"], "delegate", "the listener defers first");
        let exit = serde_json::json!({
            "stage": "post-execute",
            "leg": "exit",
            "token": 9,
            "decision": { "kind": "accept", "additionalContexts": [] },
        });
        let reply = spill_frame(
            &mut state,
            fr_wire::services::compaction::METHOD_SPILL_POST_EXECUTE,
            &exit,
        );
        assert_eq!(reply["kind"], "final");
        let decision = &reply["decision"];
        let content = decision["content"].as_array().expect("content");
        let inline = content
            .iter()
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("");
        // The rendered image line rides the artifact (the full text),
        // not the inline copy.
        let first_entry = |dir: &std::path::Path| -> std::path::PathBuf {
            dir.read_dir()
                .expect("dir reads")
                .next()
                .expect("one entry")
                .expect("entry reads")
                .path()
        };
        let artifact = first_entry(&first_entry(&root));
        let full = std::fs::read_to_string(artifact).expect("artifact reads");
        let expected = "[Image: \"/exec/shot.png\"; \"image/png\"; 4x3.";
        assert!(
            full.contains(expected),
            "the artifact renders the execution-world path: {full}"
        );
        assert!(
            inline.contains("Omitted 1 images."),
            "the whole image is omitted: {inline}"
        );
    }
}
