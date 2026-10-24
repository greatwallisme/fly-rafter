//! The C-ABI dylib plugin shell (PRD 4.3): fr-session's artifact surface
//! over the dougong dynamic channel.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer; the
//! rest of the crate stays a plain rlib. The shell owns the process-wide
//! vocabulary aggregate (PRD 7.1, 2026-10-23 Q34A ruling): plugins declare
//! their durable event types at activation time through the
//! declare-event-types method, and duplicate declarations fail closed as
//! assembly errors.
//!
//! The host serializes every foreign call per plugin instance, so the
//! vocabulary needs no locking (the same contract the ABI export-table
//! documentation gives `setup`/`call` interleaving).

use dougong_abi::{
    ABI_VERSION, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};

use crate::vocabulary::EventVocabulary;

/// The provided-service name consumers inject.
pub const SERVICE_NAME: &str = "fr-session";

/// Method selector: declare durable event type names
/// (`{"types": ["...", ...]}`).
pub const METHOD_DECLARE_EVENT_TYPES: u32 = 0;

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 1;

/// The descriptor the loader consumes: provide/inject over the dynamic
/// channel, no listeners in M1 (the store wiring is a later slice).
const DESCRIPTOR: &[u8] =
    br#"{"name":"fr-session","version":"0.1.0","provide":["fr-session"],"inject":[],"listen":{}}"#;

/// Serves one method invocation against the vocabulary: a pure
/// (JSON-in, JSON-out) function so the shell surface stays testable.
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`.
pub fn dispatch(
    method: u32,
    payload: &[u8],
    vocabulary: &mut EventVocabulary,
) -> Result<Vec<u8>, String> {
    match method {
        METHOD_DECLARE_EVENT_TYPES => {
            let request: serde_json::Value = serde_json::from_slice(payload)
                .map_err(|error| format!("declare payload must be JSON: {error}"))?;
            let types: Vec<String> = serde_json::from_value(
                request
                    .get("types")
                    .cloned()
                    .ok_or_else(|| "declare payload requires a types array".to_owned())?,
            )
            .map_err(|error| format!("declare types must be an array of strings: {error}"))?;
            vocabulary
                .declare(&types)
                .map_err(|error| error.to_string())?;
            Ok(
                serde_json::to_vec(&serde_json::json!({ "declared": types.len() }))
                    .unwrap_or_default(),
            )
        }
        METHOD_DESCRIBE => Ok(serde_json::to_vec(&serde_json::json!({
            "name": SERVICE_NAME,
            "formatVersion": crate::persist::SESSION_FORMAT_VERSION,
            "knownTypes": vocabulary.known_count(),
        }))
        .unwrap_or_default()),
        _ => Err(format!("unknown fr-session method {method}")),
    }
}

struct VocabularyState {
    vocabulary: EventVocabulary,
}

unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a `cap`-sized out-buffer and a writable
    // `len` slot (export-table contract).
    if unsafe { write_out(SERVICE_NAME.as_bytes(), out, cap, len) } {
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

unsafe extern "C" fn setup() -> *mut std::os::raw::c_void {
    let state = Box::new(VocabularyState {
        vocabulary: EventVocabulary::core(),
    });
    Box::into_raw(state).cast::<std::ffi::c_void>()
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
    // SAFETY: the host passes back a handle previously returned by `setup`
    // and not yet released via `teardown`.
    let state = unsafe { &mut *(handle.cast::<VocabularyState>()) };
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
    let reply = match dispatch(method, payload, &mut state.vocabulary) {
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
    // No listeners are declared in M1; the descriptor's listen map stays
    // empty, so the host never routes events here.
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
    drop(unsafe { Box::from_raw(handle.cast::<VocabularyState>()) });
}

/// Export table for the fr-session dylib artifact; the host resolves this
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
    use super::{METHOD_DECLARE_EVENT_TYPES, METHOD_DESCRIBE, dispatch};
    use crate::vocabulary::EventVocabulary;

    #[test]
    fn declare_accepts_new_types_and_refuses_duplicates() {
        let mut vocabulary = EventVocabulary::core();
        let payload = serde_json::json!({ "types": ["compaction/start", "compaction/end"] });
        let reply = dispatch(
            METHOD_DECLARE_EVENT_TYPES,
            payload.to_string().as_bytes(),
            &mut vocabulary,
        )
        .expect("first declare");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply, serde_json::json!({ "declared": 2 }));
        assert!(vocabulary.knows("compaction/start"));

        let duplicate = serde_json::json!({ "types": ["compaction/start"] });
        let error = dispatch(
            METHOD_DECLARE_EVENT_TYPES,
            duplicate.to_string().as_bytes(),
            &mut vocabulary,
        )
        .expect_err("duplicate");
        assert_eq!(
            error,
            "session event type \"compaction/start\" is already declared"
        );
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let mut vocabulary = EventVocabulary::core();
        let reply = dispatch(METHOD_DESCRIBE, b"{}", &mut vocabulary).expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], "fr-session");
        assert_eq!(reply["formatVersion"], 4);
        assert_eq!(reply["knownTypes"], 14);
    }

    #[test]
    fn unknown_methods_and_malformed_payloads_fail_with_text() {
        let mut vocabulary = EventVocabulary::core();
        assert!(
            dispatch(9, b"{}", &mut vocabulary)
                .expect_err("unknown method")
                .contains("unknown fr-session method 9")
        );
        assert!(
            dispatch(METHOD_DECLARE_EVENT_TYPES, b"not json", &mut vocabulary)
                .expect_err("bad json")
                .contains("declare payload must be JSON")
        );
    }
}

// Rust guideline compliant 2026-10-23
