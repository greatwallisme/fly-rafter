// Rust guideline compliant 2026-10-28
//! The C-ABI dylib plugin shell (PRD 4.5): fr-guard-timeout's artifact
//! surface over the dougong dynamic channel.
//!
//! The plugin provides the `guard-timeout` endpoint service and joins the
//! `tools` execute stage as a runtime-granted policy (D31): `setup`
//! copies the outbound vtable, grants the `tools` provider the right to
//! require this service, and registers the listener endpoint through the
//! `'tools'` service (Q101′A). The endpoint speaks the two-leg
//! arm-deadline vocabulary (F3): the enter leg answers
//! delegate-with-deadline for budgeted tools, the exit leg replaces the
//! result only when this guard's own code won the first-abort race.

use std::collections::HashMap;
use std::sync::Mutex;

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};
use fr_tools::service::{
    ArmDeadline, ExecEnter, ExecExit, ExecReply, WireEndpoint, WireOutcome, WireStage,
};
use serde_json::{Value, json};

use crate::{TOOL_TIMEOUT, tool_timeout_result};

/// The endpoint service this plugin provides.
pub const SERVICE_NAME: &str = "guard-timeout";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-guard-timeout";

/// The listener endpoint's method selector (the two-leg frame).
pub const METHOD_LISTENER: u32 = 1;

/// The base-patch configuration the dsh assembly pins (a tool that
/// declares no budget runs unbudgeted; the guard itself has no config).
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-guard-timeout\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"guard-timeout\"],\"inject\":[\"tools\"],\"listen\":{}}"
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

    /// Grants `target`'s provider the right to require `granted` (D31;
    /// fail-closed statuses surface to the caller).
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

/// The shell state: the enter-leg budgets awaiting their exit legs.
pub struct ShellState {
    /// The budget each armed conversation runs under, keyed by the
    /// continue token pairing the two legs.
    budgets: Mutex<HashMap<u64, f64>>,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState").finish_non_exhaustive()
    }
}

impl ShellState {
    /// Builds the state; `None` when the tools listener registration is
    /// declined (fail-closed activation: an assembly that does not mount
    /// the tools face, or refuses the registration, never runs unguarded).
    fn try_new(link: Option<HostLink>) -> Option<Self> {
        let link = link?;
        link.grant("tools", SERVICE_NAME).ok()?;
        let registration = json!({
            "stage": WireStage::Execute,
            "endpoint": WireEndpoint { service: SERVICE_NAME.to_owned(), method: METHOD_LISTENER },
        });
        let payload = serde_json::to_vec(&registration).ok()?;
        link.require("tools", 1, &payload).ok()?;
        Some(Self {
            budgets: Mutex::new(HashMap::new()),
        })
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
                let enter: ExecEnter = serde_json::from_value(frame)
                    .map_err(|error| format!("enter frame decode: {error}"))?;
                let reply = match enter.timeout_ms {
                    Some(ms) => {
                        self.budgets
                            .lock()
                            .expect("budget lock")
                            .insert(enter.token, ms);
                        ExecReply::Delegate {
                            arm_deadline: Some(ArmDeadline {
                                ms,
                                code: TOOL_TIMEOUT.to_owned(),
                            }),
                        }
                    }
                    None => ExecReply::Delegate { arm_deadline: None },
                };
                serde_json::to_vec(&reply).map_err(|error| error.to_string())
            }
            "exit" => {
                let exit: ExecExit = serde_json::from_value(frame)
                    .map_err(|error| format!("exit frame decode: {error}"))?;
                let budget = self
                    .budgets
                    .lock()
                    .expect("budget lock")
                    .remove(&exit.token);
                let reply = match budget {
                    Some(ms) => {
                        // Scope by code: only this guard's own deadline
                        // replaces the result; a nested outer deadline
                        // reads as an upstream cancel and keeps the
                        // result.
                        let own_timer_fired = exit
                            .deadline
                            .as_ref()
                            .and_then(|state| state.timeout_code.as_deref())
                            .is_some_and(|code| code == TOOL_TIMEOUT);
                        if own_timer_fired {
                            ExecReply::Final {
                                outcome: WireOutcome::Authored,
                                result: tool_timeout_result(ms),
                            }
                        } else {
                            ExecReply::Final {
                                outcome: WireOutcome::Canonical,
                                result: exit.result,
                            }
                        }
                    }
                    None => ExecReply::Final {
                        outcome: WireOutcome::Canonical,
                        result: exit.result,
                    },
                };
                serde_json::to_vec(&reply).map_err(|error| error.to_string())
            }
            other => Err(format!("unknown listener leg {other}")),
        }
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
/// Panics when the budget lock is poisoned by a concurrent panic.
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
    let Some(state) = ShellState::try_new(link) else {
        // Decline activation (null handle): the guard never runs unmounted
        // or unregistered.
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

const unsafe extern "C" fn on_event(
    _handle: *mut std::os::raw::c_void,
    _event: u32,
    _mode: u32,
    _in_data: *const u8,
    _in_len: usize,
    _next_available: u32,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    // No listeners are declared (the reset hook lives in the repeat
    // guard); the host never routes events here.
    STATUS_FAILED
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

/// Export table for the fr-guard-timeout dylib artifact.
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
    use super::{DESCRIPTOR, METHOD_LISTENER, PLUGIN_NAME, SERVICE_NAME, ShellState, dispatch};
    use fr_tools::service::{DeadlineState, ExecEnter, ExecExit, ExecReply, WireStage};
    use fr_tools::types::text_content;
    use serde_json::Value;

    /// A state without a host link: the endpoint logic alone is under
    /// test (the registration round trip is the fr-tools service's
    /// tested face).
    fn bare_state() -> ShellState {
        ShellState {
            budgets: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn enter(token: u64, timeout_ms: Option<f64>) -> Vec<u8> {
        serde_json::to_vec(&ExecEnter {
            stage: WireStage::Execute,
            leg: fr_tools::service::WireLeg::Enter,
            token,
            call: fr_tools::service::WireExecution {
                token: 0,
                root_call_id: Some("call-1".to_owned()),
                call_id: "call-1".to_owned(),
                name: "slow".to_owned(),
                schema: None,
                arguments: serde_json::json!({ "value": "x" }),
                agent: None,
                parent: None,
            },
            timeout_ms,
        })
        .expect("enter encodes")
    }

    fn exit(token: u64, fired: bool, code: Option<&str>) -> Vec<u8> {
        serde_json::to_vec(&ExecExit {
            stage: WireStage::Execute,
            leg: fr_tools::service::WireLeg::Exit,
            token,
            result: success_result(),
            deadline: Some(DeadlineState {
                fired,
                timeout_code: code.map(ToOwned::to_owned),
            }),
        })
        .expect("exit encodes")
    }

    fn success_result() -> fr_tools::types::ToolExecutionResult {
        fr_tools::types::ToolExecutionResult::Success(fr_tools::types::ToolExecutionSuccess {
            value: serde_json::json!("done"),
            content: text_content("done"),
            meta: None,
            additional_contexts: Vec::new(),
            concludes_turn: false,
        })
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let state = bare_state();
        let reply = dispatch(&state, 0, b"null").expect("describe");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["service"], SERVICE_NAME);
        assert_eq!(parsed["abiVersion"], 4);
    }

    #[test]
    fn descriptor_declares_grant_and_inject() {
        let parsed: Value = serde_json::from_slice(DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["provide"][0], SERVICE_NAME);
        assert_eq!(parsed["inject"], serde_json::json!(["tools"]));
        assert_eq!(parsed["listen"], serde_json::json!({}));
    }

    #[test]
    fn unbudgeted_tool_delegates_unarmed() {
        let state = bare_state();
        let reply = dispatch(&state, METHOD_LISTENER, &enter(1, None)).expect("enter");
        let parsed: ExecReply = serde_json::from_slice(&reply).expect("reply");
        assert!(matches!(parsed, ExecReply::Delegate { arm_deadline: None }));
    }

    #[test]
    fn budgeted_tool_arms_the_own_code() {
        let state = bare_state();
        let reply = dispatch(&state, METHOD_LISTENER, &enter(7, Some(25.0))).expect("enter");
        let parsed: ExecReply = serde_json::from_slice(&reply).expect("reply");
        let ExecReply::Delegate {
            arm_deadline: Some(armed),
        } = parsed
        else {
            panic!("a budgeted tool must arm its deadline")
        };
        assert_eq!((armed.ms, armed.code.as_str()), (25.0, "TOOL_TIMEOUT"));
    }

    #[test]
    fn own_expiry_replaces_the_result() {
        let state = bare_state();
        dispatch(&state, METHOD_LISTENER, &enter(7, Some(1.0))).expect("enter");
        let reply = dispatch(
            &state,
            METHOD_LISTENER,
            &exit(7, true, Some("TOOL_TIMEOUT")),
        )
        .expect("exit");
        let parsed: ExecReply = serde_json::from_slice(&reply).expect("reply");
        let ExecReply::Final {
            outcome: fr_tools::service::WireOutcome::Authored,
            result: fr_tools::types::ToolExecutionResult::Failure(failure),
        } = parsed
        else {
            panic!("an own-code expiry must author the timeout result")
        };
        assert_eq!(failure.error.message, "tool call timed out after 1ms");
    }

    #[test]
    fn foreign_code_keeps_the_result() {
        let state = bare_state();
        dispatch(&state, METHOD_LISTENER, &enter(7, Some(1000.0))).expect("enter");
        let reply = dispatch(
            &state,
            METHOD_LISTENER,
            &exit(7, true, Some("OUTER_DEADLINE")),
        )
        .expect("exit");
        let parsed: ExecReply = serde_json::from_slice(&reply).expect("reply");
        assert!(
            matches!(
                parsed,
                ExecReply::Final {
                    outcome: fr_tools::service::WireOutcome::Canonical,
                    ..
                }
            ),
            "a foreign code reads as an upstream cancel"
        );
    }

    #[test]
    fn unknown_token_passes_through_canonically() {
        // An exit without its enter (a lost conversation) keeps the
        // result: the guard never invents a timeout it did not arm.
        let state = bare_state();
        let reply = dispatch(&state, METHOD_LISTENER, &exit(404, false, None)).expect("exit");
        let parsed: ExecReply = serde_json::from_slice(&reply).expect("reply");
        assert!(matches!(
            parsed,
            ExecReply::Final {
                outcome: fr_tools::service::WireOutcome::Canonical,
                ..
            }
        ));
    }

    #[test]
    fn unknown_leg_and_method_fail_closed() {
        let state = bare_state();
        let frame = serde_json::json!({ "leg": "bogus" })
            .to_string()
            .into_bytes();
        let error = dispatch(&state, METHOD_LISTENER, &frame).expect_err("bogus leg");
        assert!(error.contains("unknown listener leg bogus"), "{error}");
        let error = dispatch(&state, 42, b"null").expect_err("unknown method");
        assert!(
            error.contains("unknown fr-guard-timeout method 42"),
            "{error}"
        );
    }
}
