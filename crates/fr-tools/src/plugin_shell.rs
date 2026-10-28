// Rust guideline compliant 2026-10-28
//! The C-ABI dylib plugin shell (PRD 4.5): fr-tools' artifact surface
//! over the dougong dynamic channel.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer of
//! the shell path. The shell provides the `tools` service (dispatch
//! surface + Q101′A listener registration); `setup` stashes the copied
//! `HostExports` vtable so the endpoint adapters call policy plugins
//! through outbound `require` under activation-scoped grants (D31), and
//! the notification mirrors (`tools/change`, `tools/result`,
//! `tools/settled`) ride outbound `emit`. `teardown` joins every
//! dispatch worker before reclaiming the state (the plugin-owned-thread
//! discipline).

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};

use crate::registry::{ToolRegistry, ToolsEmitter};
use crate::service::{HostSeam, PLUGIN_NAME, ToolEvents, ToolsService};
use crate::types::ToolPresentationMode;

pub use crate::service::SERVICE_NAME;

/// The descriptor the loader consumes: the `tools` service over the
/// dynamic channel, no injected dependencies (policy endpoints reach
/// this plugin through runtime grants, not static declarations), no
/// listeners. The version is sourced from crate metadata so it cannot
/// drift.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-tools\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"tools\"],\"inject\":[],\"listen\":{}}"
)
.as_bytes();

/// The shell state: the service over one registry.
pub struct ShellState {
    service: ToolsService,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("service", &self.service)
            .finish()
    }
}

impl ShellState {
    /// Assembles the state over one copied vtable (a null host keeps the
    /// mirrors silent and listener registration declined: no seam, no
    /// endpoints).
    #[must_use]
    pub fn new(host: Option<HostSeam>) -> Self {
        let events = std::sync::Arc::new(ToolEvents::default());
        let emitter: std::sync::Arc<dyn ToolsEmitter> =
            std::sync::Arc::clone(&events) as std::sync::Arc<dyn ToolsEmitter>;
        let registry = ToolRegistry::new(ToolPresentationMode::Native, emitter);
        let mut service = ToolsService::new(registry, events);
        if let Some(seam) = host {
            service.attach(seam);
        }
        Self { service }
    }

    /// The hosted service (tests drive registrations and the dispatch
    /// surface).
    #[must_use]
    pub const fn service(&self) -> &ToolsService {
        &self.service
    }

    /// The hosted service, mutable (tests drive the mutable serve face).
    #[must_use]
    pub const fn service_mut(&mut self) -> &mut ToolsService {
        &mut self.service
    }
}

/// Serves one method invocation against the shell state.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
pub fn dispatch(
    state: &mut ShellState,
    method: u32,
    payload: &[u8],
) -> Result<Vec<u8>, crate::service::ServiceError> {
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
    let seam = if host.is_null() {
        None
    } else {
        // SAFETY: the vtable contract guarantees validity for this call;
        // copying by value is the sanctioned pattern (dylib-plugin.md).
        Some(HostSeam::new(unsafe { *host }))
    };
    Box::into_raw(Box::new(ShellState::new(seam))).cast::<std::ffi::c_void>()
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
    // bytes; an empty payload arrives as null/0 and never dereferences
    // the pointer, which `from_raw_parts` requires non-null even for a
    // zero-length slice.
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    // SAFETY: the handle was produced by `setup` and is valid until the
    // matching `teardown`, which the host never overlaps with a call; the
    // host serializes calls per the export-table contract, so the mutable
    // borrow is exclusive.
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
    let reply = match dispatch(state, method, payload) {
        Ok(reply) => reply,
        Err(error) => {
            let body = serde_json::json!({ "error": error.0 }).to_string();
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
    // No listeners are declared; the descriptor's listen map stays empty,
    // so the host never routes events here.
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
    // once; join every dispatch worker before reclaiming (the
    // plugin-owned-thread discipline).
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
    state.service.join_workers();
    // SAFETY: the pointer came from `Box::into_raw` in `setup` and this
    // teardown is its unique reclaim.
    drop(unsafe { Box::from_raw(std::ptr::from_mut(state)) });
}

/// Export table for the fr-tools dylib artifact; the host resolves this
/// symbol by name at load time.
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
    use crate::service::METHOD_DESCRIBE;
    use serde_json::Value;

    #[test]
    fn describe_reports_the_shell_metadata() {
        let mut state = ShellState::new(None);
        let reply = dispatch(&mut state, METHOD_DESCRIBE, b"null").expect("describe");
        let parsed: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["service"], "tools");
        assert_eq!(parsed["abiVersion"], 4);
    }

    #[test]
    fn descriptor_declares_the_tools_service() {
        let parsed: Value = serde_json::from_slice(DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"][0], "tools");
        assert_eq!(parsed["inject"], serde_json::json!([]));
        assert_eq!(parsed["listen"], serde_json::json!({}));
    }

    #[test]
    fn unknown_methods_fail_through_the_error_shape() {
        let mut state = ShellState::new(None);
        let failure = dispatch(&mut state, 42, b"null").expect_err("unknown");
        assert!(
            failure.0.contains("unknown fr-tools method 42"),
            "{failure:?}"
        );
    }
}
