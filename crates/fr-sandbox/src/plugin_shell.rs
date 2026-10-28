// Rust guideline compliant 2026-10-29
//! The dylib artifact shell: descriptor, activation wiring, and the
//! export table (the fr-approval judgment shape).

use std::ffi::c_void;
use std::sync::Arc;

use dougong_abi::ABI_VERSION;
use dougong_abi::HostExports;
use dougong_abi::PluginExports;
use dougong_abi::STATUS_CAPACITY;
use dougong_abi::STATUS_FAILED;
use dougong_abi::STATUS_OK;
use dougong_abi::write_out;
use serde_json::Value;
use serde_json::json;

use crate::policy::PolicyConfig;
use crate::policy::PolicyHome;
use crate::provider::LocalSandboxProvider;
use crate::provider::ProviderConfig;
use crate::provider::SandboxInternals;
use crate::seam::SandboxMode;
use crate::service::SandboxPolicyService;
use crate::service::SessionFactsRead;

pub use crate::service::METHOD_CONFINE;
pub use crate::service::METHOD_CONTEXT_TEXT;
pub use crate::service::METHOD_DESCRIBE;
pub use crate::service::METHOD_FOLD;
pub use crate::service::METHOD_RESOLVE;
pub use crate::service::METHOD_SANDBOX_MODE;
pub use crate::service::METHOD_SEAM_DESCRIBE;
pub use crate::service::SEAM_SERVICE_NAME;
pub use crate::service::SERVICE_NAME;

/// The plugin name the host registers the artifact under.
pub const PLUGIN_NAME: &str = "fr-sandbox";

/// The `'sandboxMode'` projection's registration (dsh registers the
/// unit with `stateVersion: 1`, `init: null`).
const PROJECTION_STATE_VERSION: u64 = 1;

/// The runtime-context statement's order slot (the generated table's
/// `SANDBOX_POLICY` position; dsh `getContextOrder('SANDBOX_POLICY')`).
const SANDBOX_POLICY_ORDER: f64 = 110.0;

/// The static descriptor: two provided services (the policy home and
/// the confinement seam), three injected (all known providers, the
/// preset static-inject precedent), no listeners.
const DESCRIPTOR: &str = "{\"name\":\"fr-sandbox\",\"provide\":[\"sandbox-policy\",\"sandbox\"\
],\"inject\":[\"system-prompt\",\"session-persistence\",\"session-projections\"],\"listen\":{}}";

/// The maximum host reply the require loop grows its buffer toward (the
/// approval judgment).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// One copied host vtable (the approval `HostLink` judgment shape).
#[derive(Clone, Copy)]
pub struct HostLink {
    host: HostExports,
}

impl std::fmt::Debug for HostLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostLink").finish_non_exhaustive()
    }
}

// SAFETY: the copied vtable's raw host handle is opaque to this plugin
// and the export-table contract serializes host calls per activation;
// the approval/session shell judgments carry the same declaration.
unsafe impl Send for HostLink {}
// SAFETY: see `Send` — every require goes through the same serialized
// host entry point.
unsafe impl Sync for HostLink {}

impl HostLink {
    /// Copies one vtable.
    #[must_use]
    pub(crate) const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Grants one target activation runtime access to one of this
    /// plugin's provided services (D31).
    pub(crate) fn grant(&self, target: &str, granted: &str) -> Result<(), String> {
        let target_utf8 = target.as_bytes();
        let granted_utf8 = granted.as_bytes();
        // SAFETY: the vtable contract guarantees validity for this call.
        let status = unsafe {
            (self.host.grant)(
                self.host.host_handle,
                target_utf8.as_ptr(),
                target_utf8.len(),
                granted_utf8.as_ptr(),
                granted_utf8.len(),
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
    pub(crate) fn require(
        &self,
        service: &str,
        method: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: buffers are caller-owned per the export-table
            // contract.
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
            if status == dougong_abi::STATUS_CAPACITY {
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

/// The session-facts seam over the `'session-persistence'` read face.
struct WireSessionFacts {
    link: HostLink,
}

impl SessionFactsRead for WireSessionFacts {
    fn facts(&self, session: &str) -> Result<(Option<String>, Vec<Value>), String> {
        let payload = json!({ "id": session });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("sandbox-policy read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            &bytes,
        )?;
        let reply: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("sandbox-policy read reply decode failed: {error}"))?;
        let cwd = reply
            .pointer("/header/cwd")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let events = reply
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok((cwd, events))
    }
}

/// The shell state: the `'sandbox-policy'` service and the `'sandbox'`
/// seam provider over one copied vtable.
pub struct ShellState {
    service: SandboxPolicyService,
    provider: LocalSandboxProvider,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("service", &self.service)
            .field("provider", &self.provider)
            .finish()
    }
}

impl ShellState {
    /// Assembles the state over one copied vtable, wiring the whole
    /// registration sequence; `None` declines the activation
    /// fail-closed (any grant or registration refusal leaves the
    /// assembly without a sandbox seam rather than a half-wired one).
    #[must_use]
    pub fn try_new(link: Option<HostLink>, internals: SandboxInternals) -> Option<Self> {
        let link = link?;
        // Both callback rights ride runtime grants: the system-prompt
        // provider calls the context-text endpoint, and the
        // session-projections relay calls the pure fold endpoint.
        link.grant("system-prompt", SERVICE_NAME).ok()?;
        link.grant("session-projections", SERVICE_NAME).ok()?;

        // Declare the durable override event (the process-wide
        // vocabulary aggregate; duplicates fail closed at the session
        // service).
        let declared = json!({ "types": [crate::policy::SANDBOX_MODE_EVENT] });
        let bytes = serde_json::to_vec(&declared).ok()?;
        link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_DECLARE_EVENT_TYPES,
            &bytes,
        )
        .ok()?;

        // Register the runtime-context statement (dsh `sandbox:policy`).
        // The order is the generated table's SANDBOX_POLICY slot; the
        // wiring test below pins it against the number so drift fails
        // here first.
        let context = json!({
            "name": "sandbox:policy",
            "order": SANDBOX_POLICY_ORDER,
            "text": {
                "textProvider": {
                    "service": SERVICE_NAME,
                    "method": METHOD_CONTEXT_TEXT,
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

        // Register the `sandboxMode` projection unit (Q105′A seam;
        // Q110′A′: the fold names this plugin's pure endpoint).
        let registration = json!({
            "key": "sandboxMode",
            "stateVersion": PROJECTION_STATE_VERSION,
            "fold": { "service": SERVICE_NAME, "method": METHOD_FOLD },
        });
        let bytes = serde_json::to_vec(&registration).ok()?;
        link.require(
            fr_wire::services::session_projections::SERVICE_NAME,
            fr_wire::services::session_projections::METHOD_REGISTER,
            &bytes,
        )
        .ok()?;

        let home = PolicyHome::new(&PolicyConfig {
            mode: baked_default_mode()?,
            workspace_root: None,
        })
        .ok()?;
        let provider = LocalSandboxProvider::new(&ProviderConfig::default(), internals).ok()?;
        let sessions = Arc::new(WireSessionFacts { link });
        Some(Self {
            service: SandboxPolicyService::new(home, sessions),
            provider,
        })
    }

    /// The hosted policy service (tests drive the wire face).
    #[must_use]
    pub const fn service(&self) -> &SandboxPolicyService {
        &self.service
    }

    /// The hosted confinement provider.
    #[must_use]
    pub const fn provider(&self) -> &LocalSandboxProvider {
        &self.provider
    }
}

/// The deployment's baked default mode — the exact expression the
/// headless bundle pins for this plugin's config row (dougong delivers
/// no config to dylib artifacts, so the approval/tools/guard baking
/// judgment applies: bake the bundle's expression, read the same
/// environment at activation). An unparsable word refuses activation
/// (the dsh assembly's schema union would fail the whole load).
fn baked_default_mode() -> Option<SandboxMode> {
    baked_default_mode_from(std::env::var("FR_PERMISSION_MODE").ok().as_deref())
}

/// The pure form of the baked default (the bundle expression: unset
/// means `workspace-write`).
#[must_use]
pub fn baked_default_mode_from(mode: Option<&str>) -> Option<SandboxMode> {
    mode.map_or(Some(SandboxMode::WorkspaceWrite), SandboxMode::parse)
}

/// Serves one method invocation against the shell state: the 1000+ band
/// routes to the `'sandbox'` seam face, everything else to
/// `'sandbox-policy'` (one export table, the band judgment).
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
pub fn dispatch(state: &ShellState, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    if method >= fr_wire::services::sandbox::METHOD_DESCRIBE {
        crate::service::serve_seam(&state.provider, method, payload)
    } else {
        state.service.serve(method, payload)
    }
}

unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a `cap`-sized out-buffer and a
    // writable `len` slot (export-table contract).
    if unsafe { write_out(PLUGIN_NAME.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn descriptor(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: same host buffer contract as `plugin_name`.
    if unsafe { write_out(DESCRIPTOR.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn setup(host: *const HostExports) -> *mut c_void {
    let link = if host.is_null() {
        None
    } else {
        // SAFETY: the vtable contract guarantees validity for this call;
        // copying by value is the sanctioned pattern (dylib-plugin.md).
        Some(HostLink::new(unsafe { *host }))
    };
    let Some(state) = ShellState::try_new(link, SandboxInternals::default()) else {
        // Decline activation (null handle): a half-wired sandbox seam
        // must not exist.
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(state)).cast::<c_void>()
}

unsafe extern "C" fn call(
    handle: *mut c_void,
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
    _handle: *mut c_void,
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
    _handle: *mut c_void,
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

unsafe extern "C" fn teardown(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was produced by `setup` and torn down exactly
    // once; the provider's grant-revoke leg runs here, its cleanup
    // failures reported nowhere further (teardown is void — the dsh
    // logger-warn shape).
    let state = unsafe { Box::from_raw(handle.cast::<ShellState>()) };
    let _ignored = state.provider().dispose();
    drop(state);
}

/// Export table for the fr-sandbox dylib artifact.
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
        DESCRIPTOR, METHOD_CONTEXT_TEXT, METHOD_FOLD, METHOD_RESOLVE, METHOD_SANDBOX_MODE,
        PLUGIN_NAME, SEAM_SERVICE_NAME, SERVICE_NAME, ShellState, baked_default_mode_from,
        dispatch,
    };
    use crate::provider::ProbeVerdict;
    use crate::provider::RunnerKind;
    use crate::provider::SandboxInternals;
    use crate::service::METHOD_CONFINE;
    use crate::service::METHOD_SEAM_DESCRIBE;
    use dougong_abi::HostExports;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::Mutex;

    /// One recorded outbound call: the target service, method, and
    /// decoded payload.
    #[derive(Debug, Clone, PartialEq)]
    struct Outbound {
        service: String,
        method: u32,
        payload: Value,
    }

    /// A fake host recording every grant and require (the approval
    /// judgment: the real vtable is exercised — only the far side is
    /// scripted).
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

    fn fake_exports(host: &'static FakeHost) -> HostExports {
        HostExports {
            host_handle: std::ptr::from_ref(host)
                .cast::<std::os::raw::c_void>()
                .cast_mut(),
            require: fake_require,
            emit: fake_emit,
            grant: fake_grant,
        }
    }

    fn linked_state() -> (super::ShellState, Wiring) {
        let host = Box::leak(Box::new(FakeHost::default())) as &'static FakeHost;
        let state = ShellState::try_new(
            Some(super::HostLink::new(fake_exports(host))),
            SandboxInternals {
                probe_bwrap: Some(Arc::new(|| false)),
                probe_landlock: Some(Arc::new(|_launcher| ProbeVerdict::Full)),
                landlock_launcher: Some("/opt/fake/landlock-run".to_owned()),
                ..SandboxInternals::default()
            },
        )
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
    fn descriptor_declares_two_services_three_injects_and_no_listeners() {
        let parsed: Value = serde_json::from_str(DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["provide"], json!(["sandbox-policy", "sandbox"]));
        assert_eq!(
            parsed["inject"],
            json!([
                "system-prompt",
                "session-persistence",
                "session-projections"
            ])
        );
        assert_eq!(parsed["listen"], json!({}));
    }

    #[test]
    fn null_host_refuses_activation() {
        assert!(ShellState::try_new(None, SandboxInternals::default()).is_none());
    }

    #[test]
    fn wiring_grants_declares_registers_context_and_projection_in_order() {
        let (_state, wiring) = linked_state();
        assert_eq!(
            wiring.grants,
            vec![
                ("system-prompt".to_owned(), "sandbox-policy".to_owned()),
                (
                    "session-projections".to_owned(),
                    "sandbox-policy".to_owned()
                ),
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
                ("session-projections".to_owned(), 1000),
            ],
            "event declaration, context registration, projection registration — in order"
        );
        assert_eq!(
            wiring.requires[0].payload,
            json!({ "types": ["sandbox/mode"] })
        );
        assert_eq!(
            wiring.requires[1].payload,
            json!({
                "name": "sandbox:policy",
                "order": 110.0,
                "text": { "textProvider": { "service": SERVICE_NAME, "method": METHOD_CONTEXT_TEXT } },
            })
        );
        assert_eq!(
            wiring.requires[2].payload,
            json!({
                "key": "sandboxMode",
                "stateVersion": 1,
                "fold": { "service": SERVICE_NAME, "method": METHOD_FOLD },
            })
        );
    }

    #[test]
    fn a_refusing_registration_declines_activation() {
        struct RefusingHost;
        // SAFETY: the refusing host holds no state.
        unsafe impl Send for RefusingHost {}
        // SAFETY: same as `Send`.
        unsafe impl Sync for RefusingHost {}
        unsafe extern "C" fn refusing_require(
            _host: *mut std::os::raw::c_void,
            _service: *const u8,
            _service_len: usize,
            _method: u32,
            _in_data: *const u8,
            _in_len: usize,
            _out: *mut u8,
            _cap: usize,
            _out_len: *mut usize,
        ) -> i32 {
            dougong_abi::STATUS_FAILED
        }
        unsafe extern "C" fn quiet_emit(
            _host: *mut std::os::raw::c_void,
            _event: *const u8,
            _event_len: usize,
            _payload: *const u8,
            _payload_len: usize,
        ) -> i32 {
            dougong_abi::STATUS_OK
        }
        unsafe extern "C" fn quiet_grant(
            _host: *mut std::os::raw::c_void,
            _target: *const u8,
            _target_len: usize,
            _granted: *const u8,
            _granted_len: usize,
        ) -> i32 {
            dougong_abi::STATUS_OK
        }
        let host: &'static RefusingHost = Box::leak(Box::new(RefusingHost));
        let exports = HostExports {
            host_handle: std::ptr::from_ref(host)
                .cast::<std::os::raw::c_void>()
                .cast_mut(),
            require: refusing_require,
            emit: quiet_emit,
            grant: quiet_grant,
        };
        assert!(
            ShellState::try_new(
                Some(super::HostLink::new(exports)),
                SandboxInternals::default()
            )
            .is_none(),
            "any refused registration declines the activation"
        );
    }

    #[test]
    fn the_baked_default_maps_the_bundle_expression() {
        assert_eq!(
            baked_default_mode_from(None),
            Some(crate::seam::SandboxMode::WorkspaceWrite)
        );
        assert_eq!(
            baked_default_mode_from(Some("read-only")),
            Some(crate::seam::SandboxMode::ReadOnly)
        );
        assert_eq!(
            baked_default_mode_from(Some("danger-full-access")),
            Some(crate::seam::SandboxMode::DangerFullAccess)
        );
        assert_eq!(baked_default_mode_from(Some("root")), None);
    }

    #[test]
    fn the_policy_face_serves_through_dispatch() {
        let (state, _wiring) = linked_state();
        let describe: Value = serde_json::from_slice(
            &dispatch(&state, super::METHOD_DESCRIBE, b"{}").expect("describe"),
        )
        .expect("json");
        assert_eq!(describe["service"], SERVICE_NAME);
        let mode: Value =
            serde_json::from_slice(&dispatch(&state, METHOD_SANDBOX_MODE, b"{}").expect("mode"))
                .expect("json");
        // The baked default over the fake host is the assembly-layer
        // value (the live environment decides; the wiring above does not
        // set FR_PERMISSION_MODE).
        let expected = baked_default_mode_from(std::env::var("FR_PERMISSION_MODE").ok().as_deref())
            .map_or("workspace-write", super::super::seam::SandboxMode::as_str);
        assert_eq!(mode["mode"], expected);
    }

    #[test]
    fn the_seam_face_serves_the_1000_band_through_dispatch() {
        let (state, _wiring) = linked_state();
        let describe: Value = serde_json::from_slice(
            &dispatch(&state, METHOD_SEAM_DESCRIBE, b"{}").expect("describe"),
        )
        .expect("json");
        assert_eq!(describe["service"], SEAM_SERVICE_NAME);
        let confined: Value = serde_json::from_slice(
            &dispatch(
                &state,
                METHOD_CONFINE,
                br#"{"argv":["bash","-c","true"],"policy":{"mode":"read-only","workspace_root":"/ws"}}"#,
            )
            .expect("confine"),
        )
        .expect("json");
        assert_eq!(confined["argv"][0], "/opt/fake/landlock-run");
        assert_eq!(confined["enforcement"], "full");
    }

    #[test]
    fn unknown_methods_refuse_per_band() {
        let (state, _wiring) = linked_state();
        assert_eq!(
            dispatch(&state, 77, b"{}").unwrap_err(),
            format!("unknown {SERVICE_NAME} method 77")
        );
        assert_eq!(
            dispatch(&state, 1077, b"{}").unwrap_err(),
            format!("unknown {SEAM_SERVICE_NAME} method 1077")
        );
    }

    #[test]
    fn wire_constants_pin_the_fr_wire_table() {
        assert_eq!(
            super::METHOD_DESCRIBE,
            fr_wire::services::sandbox_policy::METHOD_DESCRIBE
        );
        assert_eq!(
            METHOD_SANDBOX_MODE,
            fr_wire::services::sandbox_policy::METHOD_SANDBOX_MODE
        );
        assert_eq!(
            METHOD_RESOLVE,
            fr_wire::services::sandbox_policy::METHOD_RESOLVE
        );
        assert_eq!(
            METHOD_CONTEXT_TEXT,
            fr_wire::services::sandbox_policy::METHOD_CONTEXT_TEXT
        );
        assert_eq!(METHOD_FOLD, fr_wire::services::sandbox_policy::METHOD_FOLD);
        assert_eq!(SEAM_SERVICE_NAME, fr_wire::services::sandbox::SERVICE_NAME);
        assert_eq!(
            METHOD_SEAM_DESCRIBE,
            fr_wire::services::sandbox::METHOD_DESCRIBE
        );
        assert_eq!(METHOD_CONFINE, fr_wire::services::sandbox::METHOD_CONFINE);
        assert_eq!(SERVICE_NAME, "sandbox-policy");
        // The band split: policy methods stay below the seam band.
    }

    #[test]
    fn the_chain_tables_stay_pinned() {
        assert_eq!(
            crate::provider::platform_chain("linux"),
            &[RunnerKind::Bwrap, RunnerKind::Landlock]
        );
        assert_eq!(
            crate::provider::platform_chain("darwin"),
            &[RunnerKind::Seatbelt]
        );
        assert_eq!(
            crate::provider::platform_chain("win32"),
            &[RunnerKind::WindowsAcl]
        );
        assert_eq!(
            crate::provider::platform_chain("sunos"),
            &[] as &[RunnerKind]
        );
    }
}
