// Rust guideline compliant 2026-10-28
//! The C-ABI dylib plugin shell (PRD 4.5): fr-approval's artifact
//! surface over the dougong dynamic channel.
//!
//! The plugin provides the `approval` service and wires itself into the
//! assembly at `setup`: it declares its three durable audit events,
//! registers the `approval:policy` runtime-context endpoint with
//! `system-prompt` (order `APPROVAL_POLICY`), and installs its request
//! endpoint as the `tools` ask seam (Q109-A) — both callback rights ride
//! runtime grants (D31), so neither provider declares this plugin's
//! service. The configured default policy is baked from
//! `FR_PERMISSION_MODE` with the exact expression the headless bundle
//! pins (dougong delivers no config to dylib artifacts; the baked-config
//! judgment the tools and guard shells already carry).

use std::sync::Arc;

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};
use serde_json::{Value, json};

use crate::service::{AnswererEndpoint, ApprovalService};
use crate::{AgentInject, Answer, Answerer, Approval, ApprovalAsk, ApprovalPolicy, SessionLog};

/// The provided-service name consumers call.
pub const SERVICE_NAME: &str = "approval";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-approval";

/// Upper bound for one outbound round trip on the growable buffer
/// protocol (the dougong host documents a 16 MiB ceiling).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// The descriptor the loader consumes: provides `approval`, injects the
/// four consumed services, no listeners (approval emits nothing on the
/// kernel bus in M1).
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-approval\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"approval\"],\"inject\":[\"system-prompt\",",
    "\"session-persistence\",\"tools\",\"agent\"],\"listen\":{}}"
)
.as_bytes();

/// One copied host vtable with the outbound arms this shell uses:
/// require (every wired seam) and grant (the two callback rights).
#[derive(Debug, Clone, Copy)]
pub struct HostLink {
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

/// The session-log seam over the `'session-persistence'` service.
struct WireSessionLog {
    link: HostLink,
}

impl SessionLog for WireSessionLog {
    fn append(&self, session: &str, type_name: &str, data: Value) -> Result<(), String> {
        let payload = json!({ "id": session, "type": type_name, "data": data });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("approval append encode failed: {error}"))?;
        self.link
            .require(
                fr_wire::services::session_persistence::SERVICE_NAME,
                fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
                &bytes,
            )
            .map(|_| ())
    }

    fn read_all(&self, session: &str) -> Result<Vec<Value>, String> {
        let payload = json!({ "id": session });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("approval read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            &bytes,
        )?;
        let reply: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("approval read reply decode failed: {error}"))?;
        Ok(reply
            .get("events")
            .cloned()
            .and_then(|events| events.as_array().cloned())
            .unwrap_or_default())
    }
}

/// The agent-notice seam over the `'agent'` service's inject face.
struct WireAgentInject {
    link: HostLink,
}

impl AgentInject for WireAgentInject {
    fn inject(&self, session: &str, message: Value) -> Result<(), String> {
        let payload = json!({ "sessionId": session, "message": message });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("approval notice encode failed: {error}"))?;
        self.link
            .require(
                fr_wire::services::agent::SERVICE_NAME,
                fr_wire::services::agent::METHOD_INJECT,
                &bytes,
            )
            .map(|_| ())
    }
}

/// One registered answerer endpoint: a synchronous require returning
/// `{answer: word}` (dsh answerer containment).
///
/// `"delegate"` passes, a vocabulary word claims, a rogue word contains
/// to the fail-closed outcome, and a transport or decode failure fails
/// the question closed.
pub struct WireAnswerer {
    link: HostLink,
    endpoint: AnswererEndpoint,
}

impl std::fmt::Debug for WireAnswerer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireAnswerer")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl WireAnswerer {
    /// Builds the adapter for one endpoint.
    #[must_use]
    pub fn for_endpoint(link: HostLink, endpoint: AnswererEndpoint) -> Arc<Self> {
        Arc::new(Self { link, endpoint })
    }
}

impl Answerer for WireAnswerer {
    fn answer(&self, ask: &ApprovalAsk) -> Result<Answer, String> {
        let payload = json!({
            "agent": ask.agent,
            "tool_name": ask.tool_name,
            "call_id": ask.call_id,
            "reason": ask.reason,
            "display_reason": ask.display_reason,
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("approval answerer encode failed: {error}"))?;
        let reply = self
            .link
            .require(&self.endpoint.service, self.endpoint.method, &bytes)?;
        let reply: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("approval answerer reply decode failed: {error}"))?;
        let word = reply
            .get("answer")
            .and_then(Value::as_str)
            .ok_or_else(|| "approval answerer reply requires an answer word".to_owned())?;
        crate::service::normalize_answer(word).map_err(|outcome| {
            // A rogue word does not reject the transport; it claims with
            // the contained outcome.
            format!("answerer word contained to {}", outcome.as_str())
        })
    }
}

/// The shell state: the `'approval'` service over one copied vtable.
pub struct ShellState {
    service: ApprovalService,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("service", &self.service)
            .finish()
    }
}

impl ShellState {
    /// Assembles the state over one copied vtable, wiring the whole
    /// registration sequence; `None` declines the activation
    /// fail-closed (any grant or registration refusal leaves the
    /// assembly without an approval seam rather than a half-wired one).
    #[must_use]
    pub fn try_new(link: Option<HostLink>) -> Option<Self> {
        let link = link?;
        // Both callback rights ride runtime grants: the system-prompt
        // provider calls the context-text endpoint, the tools provider
        // calls the request endpoint.
        link.grant("system-prompt", SERVICE_NAME).ok()?;
        link.grant("tools", SERVICE_NAME).ok()?;

        // Declare the three durable audit events (the process-wide
        // vocabulary aggregate; duplicates fail closed at the session
        // service).
        let declared = json!({
            "types": ["approval/asked", "approval/decided", "approval/policy"],
        });
        let bytes = serde_json::to_vec(&declared).ok()?;
        link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_DECLARE_EVENT_TYPES,
            &bytes,
        )
        .ok()?;

        // Register the runtime-context statement (dsh `approval:policy`).
        // The order is the generated table's APPROVAL_POLICY slot; the
        // wiring test below pins it against the number so drift fails
        // here first.
        let order = 115.0;
        let context = json!({
            "name": "approval:policy",
            "order": order,
            "text": {
                "textProvider": {
                    "service": SERVICE_NAME,
                    "method": crate::service::METHOD_CONTEXT_TEXT,
                },
            },
        });
        let bytes = serde_json::to_vec(&context).ok()?;
        link.require(
            fr_wire::services::system_prompt::SERVICE_NAME,
            fr_wire::services::system_prompt::METHOD_REGISTER_CONTEXT,
            &bytes,
        )
        .ok()?;

        // Install the ask seam on the tools pipeline (Q109-A).
        let seam = json!({
            "service": SERVICE_NAME,
            "method": crate::service::METHOD_REQUEST,
        });
        let bytes = serde_json::to_vec(&seam).ok()?;
        link.require(
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_SET_APPROVAL,
            &bytes,
        )
        .ok()?;

        let policy = baked_default_policy();
        let log = WireSessionLog { link };
        let inject = WireAgentInject { link };
        let domain = Approval::new(
            policy,
            Arc::new(log) as Arc<dyn SessionLog>,
            Arc::new(inject) as Arc<dyn AgentInject>,
        );
        let wire_link = link;
        let service = ApprovalService::new(
            domain,
            Arc::new(move |endpoint| WireAnswerer::for_endpoint(wire_link, endpoint)),
        );
        Some(Self { service })
    }

    /// The hosted service (tests drive the wire face).
    #[must_use]
    pub const fn service(&self) -> &ApprovalService {
        &self.service
    }
}

/// The deployment's baked default policy — the exact expression the
/// headless bundle pins for this plugin's config row (dougong delivers
/// no config to dylib artifacts, so the judgment the tools and guard
/// shells carry applies: bake the bundle's expression, read the same
/// environment at activation).
fn baked_default_policy() -> ApprovalPolicy {
    baked_default_policy_from(std::env::var("FR_PERMISSION_MODE").ok().as_deref())
}

/// The pure form of the baked default (the bundle expression: unset or
/// anything but `danger-full-access` means the interactive default).
#[must_use]
pub fn baked_default_policy_from(mode: Option<&str>) -> ApprovalPolicy {
    match mode {
        Some("danger-full-access") => ApprovalPolicy::Never,
        _ => ApprovalPolicy::Ask,
    }
}

/// Serves one method invocation against the shell state.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
pub fn dispatch(state: &ShellState, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    state.service.serve(method, payload)
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
        // Decline activation (null handle): a half-wired approval seam
        // must not exist.
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
    // matching `teardown`; the host serializes calls per the
    // export-table contract.
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
    // No listeners are declared; the host never routes events here.
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
    // SAFETY: the handle was produced by `setup` and torn down exactly
    // once.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}

/// Export table for the fr-approval dylib artifact.
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
    use super::{DESCRIPTOR, PLUGIN_NAME, SERVICE_NAME, ShellState, dispatch};
    use crate::service::{METHOD_CONTEXT_TEXT, METHOD_DESCRIBE, METHOD_REQUEST};
    use dougong_abi::HostExports;
    use serde_json::{Value, json};
    use std::sync::Mutex;

    /// One recorded outbound call: the target service, method, and
    /// decoded payload.
    #[derive(Debug, Clone, PartialEq)]
    struct Outbound {
        service: String,
        method: u32,
        payload: Value,
    }

    /// A fake host recording every grant and require; requires reply a
    /// fixed `{}` while grants succeed (the real vtable is exercised —
    /// only the far side is scripted).
    #[derive(Debug, Default)]
    struct FakeHost {
        grants: Mutex<Vec<(String, String)>>,
        requires: Mutex<Vec<Outbound>>,
    }

    // SAFETY: the recorded state is only touched behind its mutexes from
    // the vtable callbacks, which the ABI contract may invoke from any
    // plugin thread.
    unsafe impl Send for FakeHost {}
    // SAFETY: same mutex-only access contract as `Send`.
    unsafe impl Sync for FakeHost {}

    /// The recorded call log of one shell construction.
    #[derive(Debug)]
    struct Wiring {
        grants: Vec<(String, String)>,
        requires: Vec<Outbound>,
    }

    fn linked_state() -> (super::ShellState, Wiring) {
        let host = Box::leak(Box::new(FakeHost::default())) as &'static FakeHost;
        let exports = HostExports {
            host_handle: std::ptr::from_ref(host)
                .cast::<std::os::raw::c_void>()
                .cast_mut(),
            require: fake_require,
            emit: fake_emit,
            grant: fake_grant,
        };
        let state = ShellState::try_new(Some(super::HostLink::new(exports)))
            .expect("wiring succeeds against the scripted host");
        let wiring = Wiring {
            grants: host.grants.lock().expect("lock").clone(),
            requires: host.requires.lock().expect("lock").clone(),
        };
        (state, wiring)
    }

    unsafe extern "C" fn fake_require(
        host: *mut std::os::raw::c_void,
        service: *const u8,
        service_len: usize,
        method: u32,
        in_data: *const u8,
        in_len: usize,
        out: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the fake vtable's own contract — the handle is the
        // FakeHost reference set in `linked_state`.
        let host = unsafe { &*(host.cast::<FakeHost>()) };
        // SAFETY: the name buffer is valid for `service_len` per the
        // vtable contract.
        let service = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(service, service_len)).into_owned()
        };
        let payload: Value = if in_len == 0 || in_data.is_null() {
            Value::Null
        } else {
            // SAFETY: nonzero length with a non-null pointer, valid for
            // `in_len` bytes.
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(in_data, in_len) })
                .unwrap_or(Value::Null)
        };
        host.requires.lock().expect("lock").push(Outbound {
            service,
            method,
            payload,
        });
        let reply = br"{}";
        // SAFETY: caller-owned out-buffer contract.
        if unsafe { dougong_abi::write_out(reply, out, cap, out_len) } {
            dougong_abi::STATUS_OK
        } else {
            dougong_abi::STATUS_CAPACITY
        }
    }

    unsafe extern "C" fn fake_emit(
        _host: *mut std::os::raw::c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_OK
    }

    unsafe extern "C" fn fake_grant(
        host: *mut std::os::raw::c_void,
        target: *const u8,
        target_len: usize,
        granted: *const u8,
        granted_len: usize,
    ) -> i32 {
        // SAFETY: the fake vtable's own contract.
        let host = unsafe { &*(host.cast::<FakeHost>()) };
        // SAFETY: the name buffers are valid for their lengths per the
        // vtable contract.
        let target = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(target, target_len)).into_owned()
        };
        // SAFETY: same buffer contract.
        let granted = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(granted, granted_len)).into_owned()
        };
        host.grants.lock().expect("lock").push((target, granted));
        dougong_abi::STATUS_OK
    }

    #[test]
    fn descriptor_declares_service_injects_and_no_listeners() {
        let parsed: Value = serde_json::from_slice(DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["provide"], json!(["approval"]));
        assert_eq!(
            parsed["inject"],
            json!(["system-prompt", "session-persistence", "tools", "agent"])
        );
        assert_eq!(parsed["listen"], json!({}));
    }

    #[test]
    fn setup_wires_the_full_registration_sequence() {
        let (_state, wiring) = linked_state();
        assert_eq!(
            wiring.grants,
            vec![
                ("system-prompt".to_owned(), "approval".to_owned()),
                ("tools".to_owned(), "approval".to_owned()),
            ],
            "both callback rights ride runtime grants"
        );
        let targets: Vec<(String, u32)> = wiring
            .requires
            .iter()
            .map(|call| (call.service.clone(), call.method))
            .collect();
        assert_eq!(
            targets,
            vec![
                ("session-persistence".to_owned(), 0),
                ("system-prompt".to_owned(), 5),
                ("tools".to_owned(), 12),
            ],
            "declare events, register the context, install the ask seam — in order"
        );
        assert_eq!(
            wiring.requires[0].payload["types"],
            json!(["approval/asked", "approval/decided", "approval/policy"])
        );
        assert_eq!(wiring.requires[1].payload["name"], "approval:policy");
        assert_eq!(wiring.requires[1].payload["order"], 115.0);
        assert_eq!(
            wiring.requires[1].payload["text"]["textProvider"]["service"],
            "approval"
        );
        assert_eq!(wiring.requires[2].payload["service"], "approval");
        assert_eq!(wiring.requires[2].payload["method"], 1);
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let (state, _wiring) = linked_state();
        let reply = dispatch(&state, METHOD_DESCRIBE, b"null").expect("describe");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["service"], SERVICE_NAME);
        assert_eq!(parsed["abiVersion"], dougong_abi::ABI_VERSION);
    }

    #[test]
    fn a_null_host_declines_the_activation() {
        assert!(ShellState::try_new(None).is_none());
    }

    #[test]
    fn a_failing_registration_declines_the_activation() {
        // A require failure anywhere in the sequence must decline: the
        // fake host refuses the tools install specifically.
        #[derive(Debug, Default)]
        struct RefusingHost {
            requires: Mutex<Vec<Outbound>>,
        }
        // SAFETY: recorded state only touches its mutex.
        unsafe impl Send for RefusingHost {}
        // SAFETY: same mutex-only access contract.
        unsafe impl Sync for RefusingHost {}
        unsafe extern "C" fn refusing_require(
            host: *mut std::os::raw::c_void,
            service: *const u8,
            service_len: usize,
            method: u32,
            in_data: *const u8,
            in_len: usize,
            out: *mut u8,
            cap: usize,
            out_len: *mut usize,
        ) -> i32 {
            // SAFETY: the fake vtable's own contract.
            let host = unsafe { &*(host.cast::<RefusingHost>()) };
            // SAFETY: the name buffer is valid for `service_len` per
            // the vtable contract.
            let service = unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(service, service_len))
                    .into_owned()
            };
            host.requires.lock().expect("lock").push(Outbound {
                service: service.clone(),
                method,
                payload: Value::Null,
            });
            let _ = (in_data, in_len);
            if service == "tools" {
                return dougong_abi::STATUS_FAILED;
            }
            let reply = br"{}";
            // SAFETY: caller-owned out-buffer contract.
            if unsafe { dougong_abi::write_out(reply, out, cap, out_len) } {
                dougong_abi::STATUS_OK
            } else {
                dougong_abi::STATUS_CAPACITY
            }
        }
        unsafe extern "C" fn ok_grant(
            _host: *mut std::os::raw::c_void,
            _target: *const u8,
            _target_len: usize,
            _granted: *const u8,
            _granted_len: usize,
        ) -> i32 {
            dougong_abi::STATUS_OK
        }
        let host = Box::leak(Box::new(RefusingHost::default())) as &'static RefusingHost;
        let exports = HostExports {
            host_handle: std::ptr::from_ref(host)
                .cast::<std::os::raw::c_void>()
                .cast_mut(),
            require: refusing_require,
            emit: fake_emit,
            grant: ok_grant,
        };
        assert!(
            ShellState::try_new(Some(super::HostLink::new(exports))).is_none(),
            "a refused registration must decline the whole activation"
        );
    }

    #[test]
    fn the_service_face_runs_over_the_wired_state() {
        let (state, _wiring) = linked_state();
        // The scripted host answers every require with `{}`; the read
        // seam therefore folds an empty log, so the request refuses
        // outside an open turn with the verbatim text.
        let error = dispatch(
            &state,
            METHOD_REQUEST,
            json!({ "agent": "s1", "tool_name": "bash" })
                .to_string()
                .as_bytes(),
        )
        .expect_err("outside turn");
        assert!(error.starts_with("approval.request() outside an open turn"));
        // A bare context assembly (no agent) states nothing.
        let reply = dispatch(&state, METHOD_CONTEXT_TEXT, b"null").expect("context");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["text"], "");
    }

    #[test]
    fn the_baked_default_follows_the_bundle_expression() {
        use super::baked_default_policy_from;
        use crate::ApprovalPolicy;
        assert_eq!(
            baked_default_policy_from(None),
            ApprovalPolicy::Ask,
            "unset means the interactive default"
        );
        assert_eq!(
            baked_default_policy_from(Some("workspace-write")),
            ApprovalPolicy::Ask
        );
        assert_eq!(
            baked_default_policy_from(Some("read-only")),
            ApprovalPolicy::Ask
        );
        assert_eq!(
            baked_default_policy_from(Some("danger-full-access")),
            ApprovalPolicy::Never
        );
    }
}

// Rust guideline compliant 2026-10-28
