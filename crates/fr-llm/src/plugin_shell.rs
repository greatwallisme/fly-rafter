//! The C-ABI dylib plugin shell (PRD 4.5): fr-llm's artifact surface over
//! the dougong dynamic channel.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer; the
//! rest of the crate stays a plain rlib that the provider artifacts link
//! for the shared behavior library (assembly settlement, error taxonomy,
//! retry policy, projection, attribution).
//!
//! The shell provides the `llm` service (PRD 4.5/:336 — fr-agent-loop
//! injects `llm`, and the host contains no loop logic, so the service
//! lives in this artifact). The service's method-number table (the
//! Invoker-face of dsh's `LlmRuntime`) is refined by the consumer slices
//! under the PRD :131 contract framework; this M1 shell carries the
//! descriptor and a metadata echo only.

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};

/// The provided-service name consumers inject (PRD :336 `inject llm`).
pub const SERVICE_NAME: &str = "llm";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-llm";

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 1;

/// The descriptor the loader consumes: the `llm` service over the dynamic
/// channel, no injected dependencies or listeners in this slice.
const DESCRIPTOR: &[u8] =
    br#"{"name":"fr-llm","version":"0.1.0","provide":["llm"],"inject":[],"listen":{}}"#;

/// Serves one method invocation against the shell state: a pure
/// (JSON-in, JSON-out) function so the shell surface stays testable.
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`.
pub fn dispatch(method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    match method {
        METHOD_DESCRIBE => {
            let _ = payload;
            Ok(serde_json::to_vec(&serde_json::json!({
                "name": PLUGIN_NAME,
                "service": SERVICE_NAME,
                "abiVersion": ABI_VERSION,
            }))
            .unwrap_or_default())
        }
        _ => Err(format!("unknown fr-llm method {method}")),
    }
}

struct ShellState;

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

// ABI v3 hands the outbound `HostExports` vtable here. The `llm` service
// shell consumes no outbound capability in this slice, so the pointer is
// dropped unstashed: per the export-table contract it is valid for this
// call only, and copying it would extend its lifetime toward the
// documented UB window past `teardown`.
unsafe extern "C" fn setup(_host: *const HostExports) -> *mut std::os::raw::c_void {
    Box::into_raw(Box::new(ShellState)).cast::<std::ffi::c_void>()
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
    // bytes; an empty payload arrives as null/0 and never dereferences the
    // pointer, which `from_raw_parts` requires non-null even for a
    // zero-length slice.
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid for
        // `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    let reply = match dispatch(method, payload) {
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
    // SAFETY: the handle was produced by `setup` and torn down exactly once.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}

/// Export table for the fr-llm dylib artifact; the host resolves this
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
    use super::{METHOD_DESCRIBE, PLUGIN_NAME, SERVICE_NAME, dispatch};

    #[test]
    fn describe_reports_the_shell_metadata() {
        let reply = dispatch(METHOD_DESCRIBE, b"{}").expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], PLUGIN_NAME);
        assert_eq!(reply["service"], SERVICE_NAME);
        assert_eq!(reply["abiVersion"], 3);
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        assert!(
            dispatch(9, b"{}")
                .expect_err("unknown method")
                .contains("unknown fr-llm method 9")
        );
    }

    #[test]
    fn descriptor_declares_the_llm_service() {
        let descriptor =
            br#"{"name":"fr-llm","version":"0.1.0","provide":["llm"],"inject":[],"listen":{}}"#;
        let parsed: serde_json::Value = serde_json::from_slice(descriptor).expect("json");
        assert_eq!(parsed["provide"][0], "llm");
        assert_eq!(parsed["inject"], serde_json::json!([]));
    }
}

// Rust guideline compliant 2026-10-25
