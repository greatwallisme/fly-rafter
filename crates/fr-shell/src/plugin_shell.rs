// Rust guideline compliant 2026-10-29
//! The dylib artifact shell: descriptor, activation wiring, and the
//! export table (the fr-sandbox judgment shape).
//!
//! The activation sequence declares the two durable job lifecycle event
//! types, then assembles the registry over the `'session-persistence'`
//! append face and the executors over the `'sandbox'` confinement seam
//! and the `'sandbox-policy'` default read; any refusal declines the
//! activation fail-closed (a half-wired shell seam must not exist).

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

use crate::executors::{PolicyResolve, SandboxConfine};
use crate::jobs_local::LocalJobRegistry;
use crate::jobs_local::SessionLogAppend;
use crate::jobs_types::JOB_REGISTERED_EVENT;
use crate::jobs_types::JOB_SETTLED_EVENT;
use crate::service::ShellService;
use fr_sandbox::seam::{ConfinedArgv, SandboxExecutionPolicy, SandboxMode, SandboxPolicy};

/// The plugin name the host registers the artifact under.
pub const PLUGIN_NAME: &str = "fr-shell";

/// The static descriptor: three provided services over one export
/// table, three injected seams, no listeners.
const DESCRIPTOR: &str = "{\"name\":\"fr-shell\",\"provide\":[\"shell\",\"subprocess\",\"jobs\
\"],\"inject\":[\"session-persistence\",\"sandbox\",\"sandbox-policy\"],\"listen\":{}}";

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

/// The durable job lifecycle records over the `'session-persistence'`
/// append face (a failure contains and never blocks the registry).
struct WireSessionLog {
    link: HostLink,
}

impl SessionLogAppend for WireSessionLog {
    fn append(&self, session: &str, event_type: &str, data: &Value) -> Result<(), String> {
        let payload = json!({ "id": session, "type": event_type, "data": data });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("session log encode failed: {error}"))?;
        self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            &bytes,
        )?;
        Ok(())
    }
}

/// The confinement seam over the `'sandbox'` provider's fail-closed
/// wrap (the refusal text passes through unchanged — the executor's
/// classification reads it verbatim).
struct WireConfine {
    link: HostLink,
}

impl SandboxConfine for WireConfine {
    fn confine(&self, argv: &[String], policy: &SandboxPolicy) -> Result<ConfinedArgv, String> {
        let payload = json!({ "argv": argv, "policy": policy });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("sandbox confine encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::sandbox::SERVICE_NAME,
            fr_wire::services::sandbox::METHOD_CONFINE,
            &bytes,
        )?;
        serde_json::from_slice(&reply)
            .map_err(|error| format!("sandbox confine reply decode failed: {error}"))
    }
}

/// The default-policy seam over the `'sandbox-policy'` service.
struct WirePolicy {
    link: HostLink,
}

impl PolicyResolve for WirePolicy {
    fn resolve_policy(&self) -> Result<SandboxExecutionPolicy, String> {
        let reply = self.link.require(
            fr_wire::services::sandbox_policy::SERVICE_NAME,
            fr_wire::services::sandbox_policy::METHOD_RESOLVE,
            b"{}",
        )?;
        serde_json::from_slice(&reply)
            .map_err(|error| format!("sandbox policy reply decode failed: {error}"))
    }

    fn default_mode(&self) -> Result<SandboxMode, String> {
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
            .ok_or_else(|| "sandbox mode reply lacks mode".to_owned())?;
        SandboxMode::parse(word).ok_or_else(|| format!("unknown sandbox mode: {word}"))
    }
}

/// The shell state: the three-band service over one copied vtable.
pub struct ShellState {
    service: ShellService,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState").finish_non_exhaustive()
    }
}

impl ShellState {
    /// Assembles the state over one copied vtable, wiring the whole
    /// registration sequence; `None` declines the activation
    /// fail-closed.
    #[must_use]
    pub fn try_new(link: Option<HostLink>) -> Option<Self> {
        let link = link?;
        // The durable lifecycle vocabulary (duplicates fail closed at
        // the session service).
        let declared = json!({ "types": [JOB_REGISTERED_EVENT, JOB_SETTLED_EVENT] });
        let bytes = serde_json::to_vec(&declared).ok()?;
        link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_DECLARE_EVENT_TYPES,
            &bytes,
        )
        .ok()?;

        let sessions = Arc::new(WireSessionLog { link });
        let registry = LocalJobRegistry::new(sessions);
        let confine = Arc::new(WireConfine { link });
        let policy = Arc::new(WirePolicy { link });
        let service = ShellService::new(registry, confine, policy).ok()?;
        Some(Self { service })
    }

    /// The hosted service (tests drive the wire face).
    #[must_use]
    pub const fn service(&self) -> &ShellService {
        &self.service
    }
}

/// Serves one method invocation against the three-band face.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
pub fn dispatch(state: &ShellState, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    state.service.serve(method, payload)
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
    let Some(state) = ShellState::try_new(link) else {
        // Decline activation (null handle): a half-wired shell seam
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
    // once; the service dispose kills live ranges, settles and drops
    // every job, and joins the run threads before the vtable goes
    // undefined.
    let state = unsafe { Box::from_raw(handle.cast::<ShellState>()) };
    state.service().dispose();
    drop(state);
}

/// Export table for the fr-shell dylib artifact.
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
