// Rust guideline compliant 2026-11-05
//! The dylib plugin shell: the descriptor (listen on the loop's
//! `agent/request-error` waterfall), the activation-time event-type
//! declaration, and the CPS waterfall listener.
//!
//! The listener rules immediately and never sleeps (the 18c Q1A
//! two-stage carrier: the loop thread parks on the mailbox condvar for
//! the scheduled delay). This module is the crate's only dougong-abi
//! consumer.

use std::sync::Mutex;

use dougong_abi::{
    HostExports, STATUS_CAPACITY, STATUS_DEFERRED, STATUS_FAILED, STATUS_OK, write_out,
};
use fr_llm::generation::from_wire_retry_policy;
use fr_llm::retry_policy::RetryPolicy;

use crate::core::{self, Decision, Scheduled};
use crate::persist::{Outbound, PersistClient};
use crate::wire::{
    EVENT_LLM_RETRY, LlmRetryEventData, PLUGIN_NAME, RetryMode, decode_retry_decision,
    retry_decision_json,
};

/// The deferred ruling context (the CPS continuation's payload).
#[derive(Debug)]
enum Pending {
    /// Pass the downstream result through verbatim (the dsh `next()`).
    Passthrough,
    /// Inspect the downstream result, then rule the unbounded schedule.
    Always {
        /// The failed attempt's facts.
        view: Box<RequestView>,
        /// The route's policy (resolved from its wire form).
        policy: RetryPolicy,
        /// The policy identity key (shared by the fold and the event).
        key: String,
    },
}

/// The `agent/request-error` payload decode view (the camelCase subset
/// this plugin rules on; `failure` round-trips verbatim and the policy
/// decodes into the single-sourced fr-llm wire type).
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestView {
    /// The session whose attempt failed.
    pub session_id: String,
    /// The turn the attempt belonged to.
    pub turn: u64,
    /// The step the attempt belonged to.
    pub step: u64,
    /// The provider route that failed.
    pub provider: String,
    /// The structured failure facts.
    pub failure: fr_wire::Failure,
    /// The prepared call's declared retry policy in its wire form.
    #[serde(default)]
    pub retry_policy: Option<fr_llm::generation::WireRetryPolicy>,
}

/// A production jitter sample in the inclusive zero-to-one range (the
/// dsh `Math.random` face; deterministic tests inject values through the
/// pure core instead).
fn sample_from_uuid() -> f64 {
    let bytes = u64::from_be_bytes(
        uuid::Uuid::new_v4().as_bytes()[..8]
            .try_into()
            .expect("8 bytes"),
    );
    f64::from_bits((bytes >> 12) | 0x3FF0_0000_0000_0000) - 1.0
}

/// The folded entry for this attempt's policy, cold-reading the session
/// log once per session (the restart-rebuild arm).
fn entry_for(
    state: &mut core::CoreState,
    client: &PersistClient,
    view: &RequestView,
    key: &str,
) -> Option<core::Entry> {
    if !state.folded.contains(&view.session_id) {
        match client.read_events() {
            Ok(events) => {
                let folded = core::fold(&events, &view.session_id);
                state.table.extend(folded);
            }
            Err(error) => {
                eprintln!("fr: llm-retry: cold read failed: {error}");
            }
        }
        state.folded.insert(view.session_id.clone());
    }
    state
        .table
        .get(&(
            view.session_id.clone(),
            view.turn,
            view.step,
            view.provider.clone(),
            key.to_owned(),
        ))
        .cloned()
}

/// The in-crate state: the fold table with its cold-read registry plus
/// the deferred waterfall slot.
///
/// The table mutex serializes the fold; the pending mutex holds the
/// always-mode context between the deferred `on_event` and its
/// `on_next` (the host guarantees no interleaving foreign call in that
/// window).
#[derive(Debug)]
pub struct ShellState {
    outbound: Outbound,
    state: Mutex<core::CoreState>,
    pending: Mutex<Option<Pending>>,
}

impl ShellState {
    /// The shell over one outbound seam: empty fold, no deferred ruling.
    #[must_use]
    pub fn new(outbound: Outbound) -> Self {
        Self {
            outbound,
            state: Mutex::new(core::CoreState::default()),
            pending: Mutex::new(None),
        }
    }

    /// The plugin's dispatch face; no service is provided, so every
    /// method fails closed.
    ///
    /// # Errors
    ///
    /// Always: the plugin provides no service methods.
    pub fn dispatch(&self, method: u32, _payload: &[u8]) -> Result<Vec<u8>, String> {
        Err(format!("fr-llm-retry provides no service method {method}"))
    }
}

/// The `on_event` outcome.
///
/// Defer with the stored context, or veto with the decision reply.
enum Ruling {
    Defer(Box<Pending>),
    Reply {
        retry: u64,
        retry_id: String,
        delay_ms: f64,
    },
}

impl ShellState {
    /// Rules one failed attempt (the `on_event` body over decoded
    /// bytes).
    ///
    /// The pass-through arms defer with an empty payload (the dsh
    /// `next()`); a schedule appends `llm/retry` and vetoes with the
    /// decision (the 18c Q1A two-stage carrier).
    ///
    /// # Errors
    ///
    /// The durable-append refusal (the host surfaces it as the listener
    /// failure the driver contains).
    fn rule(&self, view: RequestView) -> Result<Ruling, String> {
        let Some(policy) = view.retry_policy.as_ref().map(from_wire_retry_policy) else {
            return Ok(Ruling::Defer(Box::new(Pending::Passthrough)));
        };
        let client = PersistClient::new(self.outbound, view.session_id.clone());
        let key = core::policy_key(&policy);
        let decision = {
            let mut core_state = self.state.lock().expect("retry state lock poisoned");
            let entry = entry_for(&mut core_state, &client, &view, &key);
            // The entry is an owned clone; the fold lock releases before
            // the ruling (the drop-tightening discipline).
            drop(core_state);
            core::decide(
                Some(&policy),
                &view.failure,
                entry.as_ref(),
                &mut || uuid::Uuid::new_v4().to_string(),
                sample_from_uuid(),
            )
        };
        match decision {
            Decision::Passthrough => Ok(Ruling::Defer(Box::new(Pending::Passthrough))),
            Decision::InspectAlways => Ok(Ruling::Defer(Box::new(Pending::Always {
                view: Box::new(view),
                policy,
                key,
            }))),
            Decision::Schedule(scheduled) => {
                append_retry_event(&client, &view, &policy, &scheduled, &key)?;
                {
                    let mut core_state = self.state.lock().expect("retry state lock poisoned");
                    record_scheduled(&mut core_state, &view, &key, &scheduled);
                }
                Ok(Ruling::Reply {
                    retry: scheduled.retry,
                    retry_id: scheduled.retry_id,
                    delay_ms: scheduled.delay_ms,
                })
            }
        }
    }
}

/// Makes one scheduled retry durable (the dsh backoff
/// append-before-wait discipline; the failure clone rides the event
/// verbatim). Called with no fold lock held — the persistence call must
/// not run under it.
fn append_retry_event(
    client: &PersistClient,
    view: &RequestView,
    policy: &RetryPolicy,
    scheduled: &Scheduled,
    key: &str,
) -> Result<(), String> {
    let max_retries = match policy {
        RetryPolicy::Normal { max_retries, .. } => Some(*max_retries),
        RetryPolicy::Always { .. } => None,
    };
    let mode = match policy {
        RetryPolicy::Normal { .. } => RetryMode::Normal,
        RetryPolicy::Always { .. } => RetryMode::Always,
    };
    let data = LlmRetryEventData {
        retry_id: scheduled.retry_id.clone(),
        turn: view.turn,
        step: view.step,
        provider: view.provider.clone(),
        mode,
        policy_key: key.to_owned(),
        retry: scheduled.retry,
        max_retries,
        delay_ms: scheduled.delay_ms,
        failure: view.failure.clone(),
    };
    client.append(
        EVENT_LLM_RETRY,
        &serde_json::to_value(&data).map_err(|error| format!("retry event encode: {error}"))?,
    )
}

/// Records the scheduled retry in the fold table (after the durable
/// append).
fn record_scheduled(
    state: &mut core::CoreState,
    view: &RequestView,
    key: &str,
    scheduled: &Scheduled,
) {
    state.table.insert(
        (
            view.session_id.clone(),
            view.turn,
            view.step,
            view.provider.clone(),
            key.to_owned(),
        ),
        core::Entry {
            retry: scheduled.retry,
            retry_id: scheduled.retry_id.clone(),
        },
    );
}

/// The `on_event` boundary wrapper.
///
/// Decode then rule; a malformed payload fails the listener, which the
/// driver contains to the default.
fn rule_into(state: &ShellState, payload: &[u8]) -> (i32, Option<Vec<u8>>) {
    let view: RequestView = match serde_json::from_slice(payload) {
        Ok(view) => view,
        Err(error) => {
            eprintln!("fr: llm-retry: request-error decode: {error}");
            return (STATUS_FAILED, None);
        }
    };
    match state.rule(view) {
        Ok(Ruling::Defer(pending)) => {
            *state.pending.lock().expect("pending lock poisoned") = Some(*pending);
            (STATUS_DEFERRED, None)
        }
        Ok(Ruling::Reply {
            retry,
            retry_id,
            delay_ms,
        }) => (
            STATUS_OK,
            Some(retry_decision_json(retry, &retry_id, delay_ms)),
        ),
        Err(message) => {
            eprintln!("fr: llm-retry: {message}");
            (STATUS_FAILED, None)
        }
    }
}

/// The `on_next` body (the CPS continuation).
///
/// Pass the downstream result through, or rule the always arm over it.
fn rule_next_into(state: &ShellState, chain_failed: bool, data: &[u8]) -> (i32, Option<Vec<u8>>) {
    let pending = state.pending.lock().expect("pending lock poisoned").take();
    let Some(pending) = pending else {
        return (STATUS_FAILED, None);
    };
    match pending {
        Pending::Passthrough => {
            if chain_failed {
                (STATUS_FAILED, None)
            } else {
                (STATUS_OK, Some(data.to_vec()))
            }
        }
        Pending::Always { view, policy, key } => {
            let downstream_retry = !chain_failed && decode_retry_decision(data).is_some();
            if downstream_retry {
                return (STATUS_OK, Some(data.to_vec()));
            }
            if chain_failed {
                eprintln!(
                    "fr: llm-retry: provider \"{}\" always policy ignored a downstream recovery failure",
                    view.provider
                );
            }
            let client = PersistClient::new(state.outbound, view.session_id.clone());
            let entry = {
                let mut core_state = state.state.lock().expect("retry state lock poisoned");
                entry_for(&mut core_state, &client, &view, &key)
            };
            match core::rule_always(
                downstream_retry,
                &policy,
                &view.failure,
                entry.as_ref(),
                &mut || uuid::Uuid::new_v4().to_string(),
                sample_from_uuid(),
            ) {
                Some(scheduled) => {
                    match append_retry_event(&client, &view, &policy, &scheduled, &key) {
                        Ok(()) => {
                            let mut core_state =
                                state.state.lock().expect("retry state lock poisoned");
                            record_scheduled(&mut core_state, &view, &key, &scheduled);
                            (
                                STATUS_OK,
                                Some(retry_decision_json(
                                    scheduled.retry,
                                    &scheduled.retry_id,
                                    scheduled.delay_ms,
                                )),
                            )
                        }
                        Err(message) => {
                            eprintln!("fr: llm-retry: {message}");
                            (STATUS_FAILED, None)
                        }
                    }
                }
                None => (STATUS_OK, Some(data.to_vec())),
            }
        }
    }
}

pub(crate) unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a cap-sized out-buffer and a writable
    // len slot (export-table contract).
    if unsafe { write_out(PLUGIN_NAME.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

pub(crate) unsafe extern "C" fn descriptor(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: same host buffer contract as plugin_name.
    if unsafe { write_out(DESCRIPTOR, out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

/// The descriptor the loader consumes: no service provided, the
/// persistence faces injected, and the loop's request-error waterfall
/// listened on (the handler id 1 names the only listener arm).
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-llm-retry\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[],\"inject\":[\"session-persistence\"],\"listen\":{\
     \"agent/request-error\":1}}"
)
.as_bytes();

// The vtable is copied by value (v3 contract). This plugin spawns no
// threads; every outbound call happens on a host-invoked foreign entry's
// thread, so the copy outlives no host state.
pub(crate) unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the host exports pointer is valid for the duration of
    // setup; the vtable is copied by value (v3 contract).
    let host = unsafe { *host };
    let outbound = Outbound::new(host);
    let state = ShellState::new(outbound);
    // Declare the durable vocabulary at activation (the Q34A ownership
    // channel; a duplicate or refusal fails the activation — the
    // fr-tools-builtin judgment).
    let client = PersistClient::new(outbound, String::new());
    if let Err(error) = client.declare() {
        eprintln!("fr: llm-retry: {error}");
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(state)).cast::<std::ffi::c_void>()
}

pub(crate) unsafe extern "C" fn call(
    handle: *mut std::os::raw::c_void,
    method: u32,
    _in_data: *const u8,
    _in_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // SAFETY: the handle was produced by setup and stays alive until
    // teardown reclaims it (export-table contract).
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    let message = match state.dispatch(method, &[]) {
        Ok(_) => String::new(),
        Err(message) => message,
    };
    let body = serde_json::json!({ "error": message }).to_string();
    // SAFETY: host buffer contract.
    let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
    STATUS_FAILED
}

// The waterfall listener boundary: decode then rule (a malformed payload
// fails the listener, which the driver contains to the default).
pub(crate) unsafe extern "C" fn on_event(
    handle: *mut std::os::raw::c_void,
    _event: u32,
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
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: in_len is nonzero and in_data non-null here, valid
        // for in_len readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    // Deferring outside a waterfall is a contract violation; fail the
    // listener (the driver contains it) instead of deferring blind.
    if next_available != 1 {
        return STATUS_FAILED;
    }
    let (status, reply) = rule_into(state, payload);
    if let Some(reply) = reply {
        // SAFETY: host buffer contract.
        if !unsafe { write_out(&reply, out, cap, out_len) } {
            return STATUS_CAPACITY;
        }
    }
    status
}

// The CPS continuation boundary: the host resolves the rest of the
// chain and re-enters with its result.
pub(crate) unsafe extern "C" fn on_next(
    handle: *mut std::os::raw::c_void,
    _event: u32,
    chain_status: i32,
    data: *const u8,
    len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // SAFETY: live handle per the export-table contract.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    let data: &[u8] = if len == 0 || data.is_null() {
        &[]
    } else {
        // SAFETY: len is nonzero and data non-null here, valid for
        // len readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(data, len) }
    };
    let chain_failed = chain_status != STATUS_OK;
    let (status, reply) = rule_next_into(state, chain_failed, data);
    if let Some(reply) = reply {
        // SAFETY: host buffer contract.
        if !unsafe { write_out(&reply, out, cap, out_len) } {
            return STATUS_CAPACITY;
        }
    }
    status
}

pub(crate) unsafe extern "C" fn teardown(handle: *mut std::os::raw::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was produced by setup and torn down exactly
    // once. No plugin-owned threads exist to join (the two-stage
    // carrier keeps every wait on the loop thread).
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}
