//! The C-ABI dylib plugin shell (PRD 4.5): the `fr-net` artifact surface
//! over the dougong dynamic channel.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer; the
//! rest of the crate stays a plain rlib housing the transport contract and
//! backend.
//!
//! The shell provides the `net` service (2026-10-27 Q77-A). The service
//! protocol over the dynamic channel (the Invoker-face that mirrors how
//! dsh providers reach the environment's `fetch`) is refined by the
//! runtime-registry slice; this M1 shell carries the descriptor and a
//! metadata echo only.

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};

/// The provided service name this plugin offers (Q77-A `provide net`).
pub const PROVIDED_SERVICE: &str = "net";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-net";

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 1;

/// The descriptor the loader consumes: the `net` service provided, nothing
/// injected, no listeners in this slice. The version is sourced from crate
/// metadata so it cannot drift from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-net\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"net\"],\"inject\":[],\"listen\":{}}"
)
.as_bytes();

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
                "provides": PROVIDED_SERVICE,
                "abiVersion": ABI_VERSION,
            }))
            .unwrap_or_default())
        }
        _ => Err(format!("unknown fr-net method {method}")),
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

// ABI v3 hands the outbound `HostExports` vtable here. This shell consumes
// no outbound capability in this slice, so the pointer is dropped
// unstashed: per the export-table contract it is valid for this call only,
// and copying it would extend its lifetime toward the documented UB window
// past `teardown`.
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

/// Export table for the fr-net dylib artifact; the host resolves this
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
    use super::{METHOD_DESCRIBE, PLUGIN_NAME, dispatch};

    #[test]
    fn describe_reports_the_shell_metadata() {
        let reply = dispatch(METHOD_DESCRIBE, b"{}").expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], PLUGIN_NAME);
        assert_eq!(reply["provides"], "net");
        assert_eq!(reply["abiVersion"], dougong_abi::ABI_VERSION);
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        assert!(
            dispatch(9, b"{}")
                .expect_err("unknown method")
                .contains("unknown fr-net method 9")
        );
    }

    #[test]
    fn descriptor_declares_the_provided_service() {
        // Parses the shipped `DESCRIPTOR` constant itself, so any drift (for
        // example a stale embedded version) fails this test.
        let parsed: serde_json::Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"], serde_json::json!(["net"]));
        assert_eq!(parsed["inject"], serde_json::json!([]));
        assert_eq!(parsed["listen"], serde_json::json!({}));
    }
}

// Rust guideline compliant 2026-10-27
