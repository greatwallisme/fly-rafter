//! The C-ABI dylib plugin shell (PRD 4.5): fr-permission-presets'
//! artifact surface over the dougong dynamic channel.
//!
//! The plugin provides the `permission-presets` service and wires itself
//! into the assembly at `setup`: it grants the session-projections relay
//! the right to call its fold endpoint (Q110′A′ — the registration's
//! fold-call leg rides the runtime grant, so fr-session's inject list
//! stays open to future registrants), reads the deployment's sandbox
//! mode over the `sandbox-policy` seam (a missing seam is the
//! unconfined-executor misconfiguration and declines the activation),
//! declares its one durable event, and registers the `permissions`
//! projection. The `session/created` listener (Q106-A) seeds every new
//! session before its first turn. The preset table and the approval
//! default are baked from the headless bundle's row (the baked-config
//! judgment: dougong delivers no config to dylib artifacts).
//!
//! The command registration itself lands with the Phase 18 fr-commands
//! ownership flip; the behavior face lives in the domain and is pinned
//! there.

use std::sync::Arc;

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};
use serde_json::{Value, json};

use crate::service::{self, PresetService};
use crate::{
    ApprovalPolicy, ApprovalSetPolicy, PROJECTION_KEY, PROJECTION_STATE_VERSION, PermissionPresets,
    SandboxMode, SandboxModeRead, SessionLog, baked_default_approval_policy, baked_default_preset,
    baked_presets,
};

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-permission-presets";

/// The `session/created` event id in the descriptor listen map.
pub const EVENT_SESSION_CREATED: u32 = 1;

/// The `session/created` event name (the Q43-A pump's mirror edge).
pub const EVENT_SESSION_CREATED_NAME: &str = "session/created";

/// Upper bound for one outbound round trip on the growable buffer
/// protocol (the dougong host documents a 16 MiB ceiling).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// The descriptor the loader consumes: provides `permission-presets`,
/// injects the four consumed services (Q104′A), and listens for the
/// session-created mirror edge.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-permission-presets\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"permission-presets\"],\"inject\":[\"sandbox-policy\",",
    "\"approval\",\"session-persistence\",\"session-projections\"],",
    "\"listen\":{\"session/created\":1}}"
)
.as_bytes();

/// One copied host vtable with the outbound arms this shell uses:
/// require (every wired seam) and grant (the relay's fold-call right).
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
            .map_err(|error| format!("permission append encode failed: {error}"))?;
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
            .map_err(|error| format!("permission read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            &bytes,
        )?;
        let reply: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("permission read reply decode failed: {error}"))?;
        Ok(reply
            .get("events")
            .cloned()
            .and_then(|events| events.as_array().cloned())
            .unwrap_or_default())
    }
}

/// The live sandbox-mode seam over the `'sandbox-policy'` service.
struct WireSandboxModeRead {
    link: HostLink,
}

impl SandboxModeRead for WireSandboxModeRead {
    fn sandbox_mode(&self) -> Result<SandboxMode, String> {
        let reply = self.link.require(
            fr_wire::services::sandbox_policy::SERVICE_NAME,
            fr_wire::services::sandbox_policy::METHOD_SANDBOX_MODE,
            b"{}",
        )?;
        let reply: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("sandbox mode reply decode failed: {error}"))?;
        let word = reply
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| "sandbox mode reply requires a mode word".to_owned())?;
        SandboxMode::parse(word)
            .ok_or_else(|| format!("sandbox mode reply carries an invalid word \"{word}\""))
    }
}

/// The notice-carrying approval write seam over the `'approval'`
/// service's set-policy face (the command path only).
struct WireApprovalSetPolicy {
    link: HostLink,
}

impl ApprovalSetPolicy for WireApprovalSetPolicy {
    fn set_policy(&self, agent: &str, policy: ApprovalPolicy) -> Result<(), String> {
        let payload = json!({ "agent": agent, "policy": policy.as_str() });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("permission set-policy encode failed: {error}"))?;
        self.link
            .require(
                fr_wire::services::approval::SERVICE_NAME,
                fr_wire::services::approval::METHOD_SET_POLICY,
                &bytes,
            )
            .map(|_| ())
    }
}

/// The shell state: the `'permission-presets'` service over one copied
/// vtable.
pub struct ShellState {
    service: PresetService,
    domain: PermissionPresets,
    log: Arc<WireSessionLog>,
    mode_read: Arc<WireSandboxModeRead>,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("service", &self.service)
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

impl ShellState {
    /// Assembles the state over one copied vtable, wiring the whole
    /// registration sequence; `None` declines the activation
    /// fail-closed (any grant, mode, validation, declaration, or
    /// registration refusal leaves the assembly without a permission
    /// seam rather than a half-wired one).
    #[must_use]
    pub fn try_new(link: Option<HostLink>) -> Option<Self> {
        let link = link?;
        // The relay's fold-call right: fr-session's activation may require
        // this plugin's fold endpoint (Q110′A′).
        link.grant(
            fr_wire::services::session_projections::SERVICE_NAME,
            service::SERVICE_NAME,
        )
        .ok()?;
        // The confining-executor check: a failed or absent mode read is
        // the unconfined composition (the domain carries the verbatim
        // refusal).
        let mode = WireSandboxModeRead { link }.sandbox_mode().ok();
        let domain = PermissionPresets::try_new(
            baked_presets(),
            baked_default_preset(),
            mode.as_ref(),
            baked_default_approval_policy(),
        )
        .ok()?;
        let declared = json!({ "types": ["permission/preset"] });
        let bytes = serde_json::to_vec(&declared).ok()?;
        link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_DECLARE_EVENT_TYPES,
            &bytes,
        )
        .ok()?;
        let registration = json!({
            "key": PROJECTION_KEY,
            "stateVersion": PROJECTION_STATE_VERSION,
            "fold": { "service": service::SERVICE_NAME, "method": service::METHOD_FOLD },
        });
        let bytes = serde_json::to_vec(&registration).ok()?;
        link.require(
            fr_wire::services::session_projections::SERVICE_NAME,
            fr_wire::services::session_projections::METHOD_REGISTER,
            &bytes,
        )
        .ok()?;
        let log = Arc::new(WireSessionLog { link });
        let mode_read = Arc::new(WireSandboxModeRead { link });
        let wired_service = PresetService::new(
            domain.clone(),
            Arc::clone(&log) as Arc<dyn SessionLog>,
            Arc::clone(&mode_read) as Arc<dyn SandboxModeRead>,
            Arc::new(WireApprovalSetPolicy { link }) as Arc<dyn crate::ApprovalSetPolicy>,
        );
        Some(Self {
            service: wired_service,
            domain,
            log,
            mode_read,
        })
    }

    /// Handles one notified event (the `session/created` seeding hook).
    ///
    /// # Errors
    ///
    /// Returns the failure the host logs for the listener.
    fn serve_event(&self, event: u32, payload: &[u8]) -> Result<(), String> {
        if event != EVENT_SESSION_CREATED {
            return Err(format!("unexpected event id {event}"));
        }
        let payload: Value = serde_json::from_slice(payload)
            .map_err(|error| format!("session/created decode: {error}"))?;
        let session = payload
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "session/created payload requires a sessionId".to_owned())?;
        self.domain
            .pin_initial_permission(session, self.log.as_ref(), self.mode_read.as_ref())
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
        // Decline activation (null handle): a half-wired permission seam
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
    // Seeding failures surface to the host (dsh pin throws at the
    // created edge); the event face carries no veto either way.
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
    // SAFETY: the handle was produced by `setup` and torn down exactly
    // once.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}

/// Export table for the fr-permission-presets dylib artifact.
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
    use super::{DESCRIPTOR, PLUGIN_NAME, ShellState, dispatch};
    use crate::service::{METHOD_CATALOG, METHOD_COMMAND, METHOD_DESCRIBE, METHOD_FOLD};
    use dougong_abi::HostExports;
    use serde_json::{Value, json};
    use std::sync::Mutex;

    /// One recorded outbound call.
    #[derive(Debug, Clone, PartialEq)]
    struct Outbound {
        service: String,
        method: u32,
        payload: Value,
    }

    /// A fake host recording every grant and require; the mode read
    /// answers `workspace-write`, everything else answers `{}`.
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

    fn exports(host: &FakeHost) -> HostExports {
        HostExports {
            host_handle: std::ptr::from_ref(host)
                .cast::<std::os::raw::c_void>()
                .cast_mut(),
            require: fake_require,
            emit: fake_emit,
            grant: fake_grant,
        }
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
        // FakeHost reference the test set.
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
            service: service.clone(),
            method,
            payload,
        });
        let reply: &[u8] = if service == "sandbox-policy"
            && method == fr_wire::services::sandbox_policy::METHOD_SANDBOX_MODE
        {
            br#"{"mode":"workspace-write"}"#
        } else {
            br"{}"
        };
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

    fn linked_state() -> (super::ShellState, Vec<(String, String)>, Vec<Outbound>) {
        let host = Box::leak(Box::new(FakeHost::default())) as &'static FakeHost;
        let state = ShellState::try_new(Some(super::HostLink::new(exports(host))))
            .expect("wiring succeeds against the scripted host");
        let wiring = host.grants.lock().expect("lock").clone();
        let requires = host.requires.lock().expect("lock").clone();
        (state, wiring, requires)
    }

    #[test]
    fn descriptor_declares_service_injects_and_listener() {
        let parsed: Value = serde_json::from_slice(DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["provide"], json!(["permission-presets"]));
        assert_eq!(
            parsed["inject"],
            json!([
                "sandbox-policy",
                "approval",
                "session-persistence",
                "session-projections"
            ])
        );
        let listen = parsed["listen"].as_object().expect("listen map");
        assert_eq!(listen.len(), 1);
        let (name, id) = listen.iter().next().expect("one entry");
        assert_eq!(name, super::EVENT_SESSION_CREATED_NAME);
        assert_eq!(*id, json!(super::EVENT_SESSION_CREATED));
    }

    #[test]
    fn setup_wires_the_full_registration_sequence() {
        let (_state, grants, requires) = linked_state();
        assert_eq!(
            grants,
            vec![(
                "session-projections".to_owned(),
                "permission-presets".to_owned()
            )],
            "the relay's fold-call right rides the runtime grant"
        );
        let targets: Vec<(String, u32)> = requires
            .iter()
            .map(|call| (call.service.clone(), call.method))
            .collect();
        assert_eq!(
            targets,
            vec![
                (
                    "sandbox-policy".to_owned(),
                    fr_wire::services::sandbox_policy::METHOD_SANDBOX_MODE
                ),
                ("session-persistence".to_owned(), 0),
                ("session-projections".to_owned(), 1000),
            ],
            "mode read, event declaration, projection registration — in order"
        );
        assert_eq!(
            requires[2].payload,
            json!({
                "key": "permissions",
                "stateVersion": 2,
                "fold": { "service": "permission-presets", "method": 2 },
            })
        );
    }

    #[test]
    fn a_null_host_declines_the_activation() {
        assert!(ShellState::try_new(None).is_none());
    }

    #[test]
    fn a_refused_registration_declines_the_activation() {
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
            _in_data: *const u8,
            _in_len: usize,
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
            if service == "session-projections" {
                return dougong_abi::STATUS_FAILED;
            }
            let reply: &[u8] = if service == "sandbox-policy" {
                br#"{"mode":"workspace-write"}"#
            } else {
                br"{}"
            };
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
            "a refused projection registration must decline the whole activation"
        );
    }

    #[test]
    fn the_service_face_runs_over_the_wired_state() {
        let (state, _grants, _requires) = linked_state();
        let reply = dispatch(&state, METHOD_DESCRIBE, b"null").expect("describe");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["name"], "permission-presets");

        let reply = dispatch(&state, METHOD_CATALOG, b"{}").expect("catalog");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["default_preset"], "workspace-write");

        let payload = json!({
            "header": { "id": "s1" },
            "inheritedEventCount": 0,
            "events": [{ "type": "permission/preset", "data": { "preset": "read-only" } }],
        });
        let reply = dispatch(&state, METHOD_FOLD, payload.to_string().as_bytes()).expect("fold");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["state"]["preset"], "read-only");
        assert_eq!(parsed["state"]["seeded"], false);

        // The command face reads the (empty) log over the wire seam and
        // settles the current preset.
        let payload = json!({ "agent": "s1", "rawInput": "" });
        let reply =
            dispatch(&state, METHOD_COMMAND, payload.to_string().as_bytes()).expect("command");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["kind"], "success");
        assert_eq!(
            parsed["text"],
            "current preset workspace-write (available: read-only, workspace-write, danger-full-access)"
        );
    }

    #[test]
    fn the_created_listener_seeds_through_the_wire_seams() {
        let host = Box::leak(Box::new(FakeHost::default())) as &'static FakeHost;
        let state = ShellState::try_new(Some(super::HostLink::new(exports(host))))
            .expect("wiring succeeds against the scripted host");
        state
            .serve_event(1, br#"{"sessionId":"s-new"}"#)
            .expect("seed runs");
        let requires = host.requires.lock().expect("lock").clone();
        // Setup drove three requires; the seed read comes next.
        assert_eq!(
            requires[3].service,
            "session-persistence".to_owned(),
            "the seed reads the log first"
        );
        let appends: Vec<Outbound> = requires
            .iter()
            .filter(|call| {
                call.method == fr_wire::services::session_persistence::METHOD_SESSION_APPEND
            })
            .cloned()
            .collect();
        assert_eq!(
            appends.len(),
            3,
            "the fresh-session seed drives three appends"
        );
        assert_eq!(appends[0].payload["id"], "s-new");
        assert_eq!(appends[0].payload["type"], "permission/preset");
        assert_eq!(
            appends[0].payload["data"],
            json!({ "preset": "workspace-write" })
        );
        assert_eq!(appends[1].payload["type"], "sandbox/mode");
        assert_eq!(
            appends[1].payload["data"],
            json!({ "mode": "workspace-write" })
        );
        assert_eq!(appends[2].payload["type"], "approval/policy");
        assert_eq!(appends[2].payload["data"], json!({ "policy": "ask" }));

        // An unexpected event id refuses.
        assert!(state.serve_event(7, b"{}").is_err());
    }
}

// Rust guideline compliant 2026-10-29
