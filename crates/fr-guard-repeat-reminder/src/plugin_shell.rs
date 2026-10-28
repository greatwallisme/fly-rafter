// Rust guideline compliant 2026-10-28
//! The C-ABI dylib plugin shell (PRD 4.5): fr-guard-repeat-reminder's
//! artifact surface over the dougong dynamic channel.
//!
//! The plugin provides the `guard-repeat-reminder` endpoint service,
//! joins the `tools` post-execute stage as a runtime-granted policy
//! (D31 + Q101′A), and subscribes to the `agent/user-admitted`
//! notification (Q102′A): a user interjection resets the per-agent
//! repeat chain, exactly the dsh `agent/pre-step` reset hook. The
//! endpoint speaks the two-leg vocabulary: the enter leg counts the
//! call (before the downstream outcome, denied calls included), the
//! exit leg folds the reminder onto the downstream decision.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};
use fr_timeout::DeadlineSignal;
use fr_tools::service::{WireEndpoint, WireExecution, WireStage};
use fr_tools::signal::ToolSignal;
use fr_tools::types::ToolExecution;
use fr_wire::Message;
use serde_json::{Value, json};

use crate::{RepeatGuard, fold_reminder};

/// The endpoint service this plugin provides.
pub const SERVICE_NAME: &str = "guard-repeat-reminder";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-guard-repeat-reminder";

/// The listener endpoint's method selector (the two-leg frame).
pub const METHOD_LISTENER: u32 = 1;

/// The `agent/user-admitted` event id in the descriptor listen map.
pub const EVENT_USER_ADMITTED: u32 = 1;

/// The `agent/user-admitted` event name (Q102′A).
pub const EVENT_USER_ADMITTED_NAME: &str = "agent/user-admitted";

/// The base-patch configuration dsh pins (cordis.patch.yml:448).
const BASE_CONFIG: &[u8] = br#"{"thresholds":[3,5,8],"argumentsPreviewChars":500}"#;

const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-guard-repeat-reminder\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"guard-repeat-reminder\"],\"inject\":[\"tools\"],",
    "\"listen\":{\"agent/user-admitted\":1}}"
)
.as_bytes();

/// Upper bound for one outbound round trip on the growable buffer
/// protocol (the dougong host documents a 16 MiB ceiling).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// One copied host vtable with the two outbound arms this shell uses:
/// require (the listener registration) and grant (the runtime capability
/// that lets the `tools` provider call back).
#[derive(Debug, Clone, Copy)]
struct HostLink {
    host: HostExports,
}

// SAFETY: the ABI vtable contract allows require/grant from any plugin
// thread, concurrently; the copy stays valid until this activation's
// `teardown` returns, and this shell never outlives its activation.
unsafe impl Send for HostLink {}

// SAFETY: sharing one copy only issues concurrent vtable calls, which the
// same contract explicitly allows.
unsafe impl Sync for HostLink {}

impl HostLink {
    /// Wraps a vtable copied at `setup`.
    const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Grants `target`'s provider the right to require `granted` (D31).
    fn grant(&self, target: &str, granted: &str) -> Result<(), String> {
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

    /// Performs one synchronous cross-plugin call (growable buffer
    /// protocol).
    fn require(&self, service: &str, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: same vtable lifetime contract as `grant`; buffers
            // are caller-owned per the export-table contract.
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
                    return Err(format!("reply still exceeds {MAX_REPLY_CAPACITY} bytes"));
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            return Err(format!(
                "require {service}.{method} failed with host status {status}: {}",
                String::from_utf8_lossy(&buf)
            ));
        }
    }
}

/// The shell state: the guard plus the reminders awaiting their folds.
pub struct ShellState {
    /// The advisory guard itself (per-agent chains, thresholds).
    guard: Arc<RepeatGuard>,
    /// The reminder the enter leg produced, keyed by the continue token
    /// pairing the two legs.
    pending: Mutex<HashMap<u64, Option<Message>>>,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("guard", &self.guard)
            .finish_non_exhaustive()
    }
}

impl ShellState {
    /// Builds the state over one guard (no host interaction; the test and
    /// activation paths share it).
    fn state_for(guard: Arc<RepeatGuard>) -> Self {
        Self {
            guard,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Builds the activated state; `None` when the guard configuration is
    /// invalid or the tools listener registration is declined
    /// (fail-closed activation: a guard that cannot mount never advises).
    fn try_new(link: Option<HostLink>, config: &[u8]) -> Option<Self> {
        let config: Value = serde_json::from_slice(config).ok()?;
        let guard = RepeatGuard::try_new(&config).ok()?;
        let link = link?;
        link.grant("tools", SERVICE_NAME).ok()?;
        let registration = json!({
            "stage": WireStage::PostExecute,
            "endpoint": WireEndpoint { service: SERVICE_NAME.to_owned(), method: METHOD_LISTENER },
        });
        let payload = serde_json::to_vec(&registration).ok()?;
        link.require("tools", 1, &payload).ok()?;
        Some(Self::state_for(guard))
    }

    /// Handles one notified event (the reset hook).
    ///
    /// # Errors
    ///
    /// Returns the failure the host logs for the listener.
    fn serve_event(&self, event: u32, payload: &[u8]) -> Result<(), String> {
        if event != EVENT_USER_ADMITTED {
            return Err(format!("unexpected event id {event}"));
        }
        let frame: Value =
            serde_json::from_slice(payload).map_err(|error| format!("event decode: {error}"))?;
        let agent = frame["agent"].as_str().unwrap_or_default();
        self.guard.reset_chain(agent);
        Ok(())
    }

    /// Serves one two-leg frame.
    ///
    /// # Errors
    ///
    /// Returns the failure the host surfaces as `STATUS_FAILED`.
    fn serve_listener(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
        let frame: Value =
            serde_json::from_slice(payload).map_err(|error| format!("frame decode: {error}"))?;
        let leg = frame["leg"].as_str().unwrap_or_default();
        match leg {
            "enter" => {
                let enter: fr_tools::service::PostEnter = serde_json::from_value(frame)
                    .map_err(|error| format!("enter frame decode: {error}"))?;
                let exec = execution_of(&enter.call);
                let reminder = self.guard.observe(&exec);
                self.pending
                    .lock()
                    .expect("pending lock")
                    .insert(enter.token, reminder);
                serde_json::to_vec(&fr_tools::service::PostReply::Delegate)
                    .map_err(|error| error.to_string())
            }
            "exit" => {
                let exit: fr_tools::service::PostExit = serde_json::from_value(frame)
                    .map_err(|error| format!("exit frame decode: {error}"))?;
                let reminder = self
                    .pending
                    .lock()
                    .expect("pending lock")
                    .remove(&exit.token)
                    .flatten();
                let decision = fold_reminder(exit.decision, reminder);
                serde_json::to_vec(&fr_tools::service::PostReply::Final { decision })
                    .map_err(|error| error.to_string())
            }
            other => Err(format!("unknown listener leg {other}")),
        }
    }
}

/// Rebuilds the observation view of one wire execution (the guard reads
/// agent, name, and arguments only; the signal is a fresh leaf).
fn execution_of(call: &WireExecution) -> ToolExecution {
    ToolExecution {
        token: call.token,
        root_call_id: call.root_call_id.clone().unwrap_or_default(),
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        schema: call.schema.clone(),
        arguments: call.arguments.clone(),
        agent: call.agent.clone(),
        parent: call.parent,
        signal: ToolSignal::plain(DeadlineSignal::new()),
    }
}

/// Serves one method invocation against the shell state.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
///
/// # Panics
///
/// Panics when the pending-reminder lock is poisoned by a concurrent
/// panic.
pub fn dispatch(state: &ShellState, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    match method {
        0 => Ok(serde_json::to_vec(&json!({
            "name": PLUGIN_NAME,
            "service": SERVICE_NAME,
            "abiVersion": ABI_VERSION,
        }))
        .expect("describe is a fixed JSON object")),
        METHOD_LISTENER => state.serve_listener(payload),
        other => Err(format!("unknown {PLUGIN_NAME} method {other}")),
    }
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

unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    let link = if host.is_null() {
        None
    } else {
        // SAFETY: the vtable contract guarantees validity for this call;
        // copying by value is the sanctioned pattern (dylib-plugin.md).
        Some(HostLink::new(unsafe { *host }))
    };
    let Some(state) = ShellState::try_new(link, BASE_CONFIG) else {
        // Decline activation (null handle): an invalid guard config or a
        // declined registration never advises.
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(state)).cast::<std::ffi::c_void>()
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
    // The host contract delivers `in_data` valid for `in_len` readable
    // bytes; an empty payload arrives as null/0.
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    // SAFETY: the handle was produced by `setup` and is valid until the
    // matching `teardown`; the host serializes calls per the export-table
    // contract.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    let reply = match dispatch(state, method, payload) {
        Ok(reply) => reply,
        Err(text) => {
            let body = json!({ "error": text }).to_string();
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

unsafe extern "C" fn on_event(
    handle: *mut std::os::raw::c_void,
    event: u32,
    _mode: u32,
    in_data: *const u8,
    in_len: usize,
    _next_available: u32,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // The host contract delivers `in_data` valid for `in_len` readable
    // bytes; an empty payload arrives as null/0.
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    // SAFETY: the handle was produced by `setup` and is valid until the
    // matching `teardown`.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    // The reset hook is pure notification: a malformed payload logs an
    // error and never vetoes anything (the event face is emit-mode).
    match state.serve_event(event, payload) {
        Ok(()) => STATUS_OK,
        Err(_) => STATUS_FAILED,
    }
}

const unsafe extern "C" fn on_next(
    _handle: *mut std::os::raw::c_void,
    _event: u32,
    _chain_status: i32,
    _data: *const u8,
    _len: usize,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    STATUS_FAILED
}

unsafe extern "C" fn teardown(handle: *mut std::os::raw::c_void) {
    if handle.is_null() {
        return;
    }
    // The registration lives in the tools service for the process
    // lifetime (teardown-only artifacts, LIFO unload order); reclaim the
    // state. SAFETY: the handle was produced by `setup` and torn down
    // exactly once.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}

/// Export table for the fr-guard-repeat-reminder dylib artifact.
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
        EVENT_USER_ADMITTED, METHOD_LISTENER, PLUGIN_NAME, SERVICE_NAME, ShellState, dispatch,
    };
    use crate::RepeatGuard;
    use fr_tools::service::{PostExit, PostReply, WireExecution, WireLeg, WireStage};
    use fr_tools::types::{PostToolDecision, text_content};
    use serde_json::{Value, json};

    /// A state over a threshold-2 guard (the minimum dsh allows).
    fn state() -> ShellState {
        let guard = RepeatGuard::try_new(&json!({ "thresholds": [2] })).expect("config");
        ShellState::state_for(guard)
    }

    fn wire_call(name: &str, arguments: Value, agent: Option<&str>) -> WireExecution {
        WireExecution {
            token: 0,
            root_call_id: Some("call-1".to_owned()),
            call_id: "call-1".to_owned(),
            name: name.to_owned(),
            schema: None,
            arguments,
            agent: agent.map(ToOwned::to_owned),
            parent: None,
        }
    }

    fn enter(token: u64, name: &str, arguments: Value, agent: Option<&str>) -> Vec<u8> {
        serde_json::to_vec(&fr_tools::service::PostEnter {
            stage: WireStage::PostExecute,
            leg: WireLeg::Enter,
            token,
            call: wire_call(name, arguments, agent),
            result: fr_tools::types::ToolExecutionResult::Success(
                fr_tools::types::ToolExecutionSuccess {
                    value: json!("done"),
                    content: text_content("done"),
                    meta: None,
                    additional_contexts: Vec::new(),
                    concludes_turn: false,
                },
            ),
        })
        .expect("enter encodes")
    }

    fn exit(token: u64) -> Vec<u8> {
        serde_json::to_vec(&PostExit {
            stage: WireStage::PostExecute,
            leg: WireLeg::Exit,
            token,
            decision: PostToolDecision::Accept {
                content: None,
                value: None,
                additional_contexts: Vec::new(),
            },
        })
        .expect("exit encodes")
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let state = state();
        let reply = dispatch(&state, 0, b"null").expect("describe");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["service"], SERVICE_NAME);
        assert_eq!(parsed["abiVersion"], 4);
    }

    #[test]
    fn descriptor_declares_listen_and_inject() {
        let parsed: Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["provide"][0], SERVICE_NAME);
        assert_eq!(parsed["inject"], serde_json::json!(["tools"]));
        assert_eq!(
            parsed["listen"]["agent/user-admitted"],
            serde_json::json!(1)
        );
    }

    #[test]
    fn un_agented_call_never_reminds() {
        let state = state();
        dispatch(
            &state,
            METHOD_LISTENER,
            &enter(1, "echo", json!({ "value": "x" }), None),
        )
        .expect("enter");
        let reply = dispatch(&state, METHOD_LISTENER, &exit(1)).expect("exit");
        let parsed: PostReply = serde_json::from_slice(&reply).expect("reply");
        let PostReply::Final {
            decision:
                PostToolDecision::Accept {
                    additional_contexts,
                    ..
                },
        } = parsed
        else {
            panic!("an accept decision must stay an accept decision")
        };
        assert!(
            additional_contexts.is_empty(),
            "a call with no agent never reminds"
        );
    }

    #[test]
    fn repeat_hits_threshold_and_folds() {
        let state = state();
        let same = |token: u64| enter(token, "echo", json!({ "value": "x" }), Some("agent-1"));
        let folded_contexts = |state: &ShellState, token: u64| -> usize {
            let reply = dispatch(state, METHOD_LISTENER, &exit(token)).expect("exit");
            let parsed: PostReply = serde_json::from_slice(&reply).expect("reply");
            let PostReply::Final {
                decision:
                    PostToolDecision::Accept {
                        additional_contexts,
                        ..
                    },
            } = parsed
            else {
                panic!("an accept decision must stay an accept decision")
            };
            additional_contexts.len()
        };
        // The first call is below threshold; the second hits it and folds
        // the gentle reminder.
        dispatch(&state, METHOD_LISTENER, &same(1)).expect("enter");
        assert_eq!(folded_contexts(&state, 1), 0);
        dispatch(&state, METHOD_LISTENER, &same(2)).expect("enter");
        assert_eq!(folded_contexts(&state, 2), 1);
    }

    #[test]
    fn user_admitted_resets_the_chain() {
        let state = state();
        let same = |token: u64| enter(token, "echo", json!({ "value": "x" }), Some("agent-1"));
        let folded_contexts = |state: &ShellState, token: u64| -> usize {
            let reply = dispatch(state, METHOD_LISTENER, &exit(token)).expect("exit");
            let parsed: PostReply = serde_json::from_slice(&reply).expect("reply");
            let PostReply::Final {
                decision:
                    PostToolDecision::Accept {
                        additional_contexts,
                        ..
                    },
            } = parsed
            else {
                panic!("an accept decision must stay an accept decision")
            };
            additional_contexts.len()
        };

        // Count 2 hits threshold 2 and reminds; count 3 is off-threshold
        // and stays silent (the unreset contrast).
        dispatch(&state, METHOD_LISTENER, &same(1)).expect("enter");
        dispatch(&state, METHOD_LISTENER, &same(2)).expect("enter");
        assert_eq!(folded_contexts(&state, 2), 1);
        dispatch(&state, METHOD_LISTENER, &same(3)).expect("enter");
        assert_eq!(folded_contexts(&state, 3), 0);

        // The user interjection resets the chain: without it the next
        // count would be 4 (off-threshold, silent); after the reset the
        // second fresh call hits threshold 2 again and reminds.
        state
            .serve_event(EVENT_USER_ADMITTED, br#"{ "agent": "agent-1" }"#)
            .expect("event");
        dispatch(&state, METHOD_LISTENER, &same(4)).expect("enter");
        assert_eq!(folded_contexts(&state, 4), 0);
        dispatch(&state, METHOD_LISTENER, &same(5)).expect("enter");
        assert_eq!(folded_contexts(&state, 5), 1);
    }

    #[test]
    fn unknown_event_id_is_rejected() {
        let state = state();
        let error = state
            .serve_event(42, br#"{ "agent": "agent-1" }"#)
            .expect_err("foreign event");
        assert!(error.contains("unexpected event id 42"), "{error}");
    }
}
