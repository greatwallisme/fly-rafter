//! The C-ABI dylib plugin shell (PRD 4.5): the `fr-llm-deepseek` artifact
//! surface over the dougong dynamic channel.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer; the
//! rest of the crate stays a plain rlib housing the provider behavior
//! (wire types, serialization, SSE translation, replay, credentials).
//!
//! The shell injects the `llm` service (PRD 4.5/:336 — dsh `llm-deepseek`
//! registers its adapter into the LLM runtime). The registration protocol
//! over the dynamic channel (the Invoker-face of dsh's
//! `registerDeepSeekProvider`) is refined by the runtime-registry slice
//! under the PRD :131 contract framework; this M1 shell carries the
//! descriptor, the B2 generation trio (Q84-B2, Q85-C: the same fr-llm
//! helper hooked per provider shell), and the adapter-registration seam
//! the runtime-registry slice owns.

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};

/// The injected service name this shell consumes (PRD :336 `inject llm`).
pub const INJECTED_SERVICE: &str = "llm";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-llm-deepseek";

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 1;

pub use fr_llm::generation::{METHOD_ABORT, METHOD_BEGIN, METHOD_DRAIN, METHOD_PREPARE_CALL};

/// The descriptor the loader consumes: no provided service, the `llm`
/// service injected over the dynamic channel, no listeners in this slice.
/// The version is sourced from crate metadata so it cannot drift from the
/// artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-llm-deepseek\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[],\"inject\":[\"llm\"],\"listen\":{}}"
)
.as_bytes();

/// Serves one method invocation against the shell state: a pure
/// (JSON-in, JSON-out) function so the shell surface stays testable.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
pub fn dispatch(state: &ShellState, method: u32, payload: &[u8]) -> Result<Vec<u8>, ShellFailure> {
    match method {
        METHOD_DESCRIBE => {
            let _ = payload;
            Ok(serde_json::to_vec(&serde_json::json!({
                "name": PLUGIN_NAME,
                "injects": INJECTED_SERVICE,
                "abiVersion": ABI_VERSION,
            }))
            .unwrap_or_default())
        }
        METHOD_PREPARE_CALL | METHOD_BEGIN | METHOD_DRAIN | METHOD_ABORT => {
            match fr_llm::generation::serve(&state.service, method, payload) {
                Ok(reply) => Ok(reply),
                Err(fr_llm::generation::ServiceError::Llm(failure)) => {
                    // The failure IS the reply body (STATUS_FAILED).
                    match serde_json::to_vec(&serde_json::json!({ "llmError": failure.failure() }))
                    {
                        Ok(body) => Err(ShellFailure::Llm(body)),
                        Err(error) => Err(ShellFailure::Text(error.to_string())),
                    }
                }
                Err(fr_llm::generation::ServiceError::Text(text)) => Err(ShellFailure::Text(text)),
            }
        }
        _ => Err(ShellFailure::Text(format!(
            "unknown fr-llm-deepseek method {method}"
        ))),
    }
}

/// The shell state: the generation service plus the copied emit vtable
/// its notification seam publishes `llm/chunk-ready` through.
pub struct ShellState {
    service: fr_llm::generation::GenerationService,
}

impl ShellState {
    /// Assembles the state over one copied vtable (the emit seam; a null
    /// host keeps the quiet seam).
    #[must_use]
    pub fn new(host: Option<fr_llm::generation::EmitVtable>) -> Self {
        let notify = host.map_or_else(fr_llm::generation::quiet_notify, |vtable| {
            vtable.notify_seam()
        });
        Self {
            service: fr_llm::generation::GenerationService::new(notify),
        }
    }

    /// The hosted service (tests register the crate's adapter and drive
    /// the trio; production registration is the runtime-registry slice).
    #[must_use]
    pub const fn service(&self) -> &fr_llm::generation::GenerationService {
        &self.service
    }
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("service", &self.service)
            .finish()
    }
}

/// One failed method call: `Llm` carries the taxonomy document the shell
/// replies as the `STATUS_FAILED` body verbatim; `Text` failures wrap as
/// `{"error": ...}`.
#[derive(Debug)]
pub enum ShellFailure {
    /// The `{"llmError": ...}` document (already JSON bytes).
    Llm(Vec<u8>),
    /// A fabric protocol failure.
    Text(String),
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

// ABI v3 hands the outbound `HostExports` vtable here. The generation
// hub's workers publish `llm/chunk-ready` through a copy of it (the
// outbound.rs judgment: the copy stays valid until this activation's
// `teardown` returns, and the hub joins every worker before then).
unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    let emit = if host.is_null() {
        None
    } else {
        // SAFETY: the vtable contract guarantees validity for this call;
        // copying by value is the sanctioned pattern (dylib-plugin.md).
        Some(fr_llm::generation::EmitVtable::new(unsafe { *host }))
    };
    Box::into_raw(Box::new(ShellState::new(emit))).cast::<std::ffi::c_void>()
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
    // SAFETY: the handle was produced by `setup` and is valid until the
    // matching `teardown`, which the host never overlaps with a call.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    let reply = match dispatch(state, method, payload) {
        Ok(reply) => reply,
        Err(ShellFailure::Llm(body)) => {
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(&body, out, cap, out_len) };
            return STATUS_FAILED;
        }
        Err(ShellFailure::Text(message)) => {
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
    // Join every hub worker before the vtable lifetime ends (the dylib
    // thread discipline), then reclaim the state.
    // SAFETY: the handle was produced by `setup` and torn down exactly once.
    let state = unsafe { Box::from_raw(handle.cast::<ShellState>()) };
    state.service.shutdown();
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
    use super::{METHOD_BEGIN, METHOD_DESCRIBE, PLUGIN_NAME, ShellState, dispatch};

    #[test]
    fn describe_reports_the_shell_metadata() {
        let state = ShellState::new(None);
        let reply = dispatch(&state, METHOD_DESCRIBE, b"{}").expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], PLUGIN_NAME);
        assert_eq!(reply["injects"], "llm");
        assert_eq!(reply["abiVersion"], 3);
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        let state = ShellState::new(None);
        assert!(matches!(
            dispatch(&state, 9, b"{}").expect_err("unknown method"),
            super::ShellFailure::Text(ref text) if text.contains("unknown fr-llm-deepseek method 9")
        ));
    }

    /// The trio reaches the shared hub through this shell's dispatch:
    /// an unregistered route replies the `llmError` document (the
    /// fr-llm shell judgment).
    #[test]
    fn begin_without_a_route_replies_the_llm_error_document() {
        let state = ShellState::new(None);
        let begin = br#"{"request":{"provider":"missing","model":"m","messages":[]}}"#;
        let body = match dispatch(&state, METHOD_BEGIN, begin).expect_err("llm failure") {
            super::ShellFailure::Llm(body) => body,
            super::ShellFailure::Text(_) => panic!("expected the llm error document"),
        };
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(parsed["llmError"]["code"], "NO_ADAPTER");
    }

    #[test]
    fn descriptor_declares_the_injected_service() {
        // Parses the shipped `DESCRIPTOR` constant itself, so any drift (for
        // example a stale embedded version) fails this test.
        let parsed: serde_json::Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"], serde_json::json!([]));
        assert_eq!(parsed["inject"][0], "llm");
        assert_eq!(parsed["listen"], serde_json::json!({}));
    }
}

// Rust guideline compliant 2026-10-26
