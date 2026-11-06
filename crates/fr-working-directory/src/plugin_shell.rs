// Rust guideline compliant 2026-11-05
//! The dylib plugin shell: the descriptor, the activation-time
//! registrations, and the service dispatch.
//!
//! The descriptor provides `working-directory`, injects the five
//! consumed faces, and declares no listeners; the activation-time
//! registrations cover the durable change vocabulary, the projection
//! fold, and the prompt context.
//!
//! The activation declines fail-closed when any registration refuses
//! (the fr-llm-retry judgment): a half-wired working-directory owner
//! must not exist. The launch directory bakes the dsh omitted-value
//! default (`process.cwd()`; the dsh `defaultDirectory` config has no
//! dylib runtime channel — the fr-shell key-table carrier judgment).
//!
//! This module is the crate's only dougong-abi consumer.

use std::sync::Arc;

use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out};
use serde_json::{Value, json};

use crate::core::{ColdLog, ProjectionState, WorkingDirectories, fold};
use crate::outbound::{
    Outbound, WireAgentInject, WireDirectoryLeg, WireSessionLog, declare_change_event,
    register_context, register_projection,
};
use crate::wire::{
    METHOD_CONTEXT_TEXT, METHOD_DESCRIBE, METHOD_ENSURE, METHOD_FOLD, METHOD_GET, METHOD_SET,
    PLUGIN_NAME, SERVICE_NAME,
};

/// The descriptor the loader consumes: provides `working-directory`,
/// injects the four declared services, no listeners.
///
/// The dsh `fs` inject rides the owner's D31 grant instead of a
/// declaration: `fs` is provided by fr-tools-builtin, which declares
/// `working-directory` — a declared inject would close the declaration
/// cycle tools-builtin -> working-directory -> fs(provider
/// tools-builtin) and gate both activations (the 2026-11-04 cycle-cut
/// ruling; the fs require passes through the grant, whose target is
/// committed first by this exact declaration edge).
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-working-directory\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"working-directory\"],\"inject\":[",
    "\"session-persistence\",\"agent\",\"session-projections\",",
    "\"system-prompt\"],\"listen\":{}}"
)
.as_bytes();

/// The shell state: the owned working-directory domain.
pub struct ShellState {
    domain: WorkingDirectories,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState").finish_non_exhaustive()
    }
}

impl ShellState {
    /// The shell over one owned domain.
    #[must_use]
    pub const fn new(domain: WorkingDirectories) -> Self {
        Self { domain }
    }

    /// Serves one method invocation.
    ///
    /// # Errors
    ///
    /// The payload decode refusal, an unknown method, or the domain
    /// refusal (the host surfaces it as the `{"error": <text>}`
    /// `STATUS_FAILED` detail the fr-approval carrier spells).
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let request: Value = if payload.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(payload)
                .map_err(|error| format!("working-directory payload must be JSON: {error}"))?
        };
        let reply = match method {
            METHOD_DESCRIBE => json!({
                "name": PLUGIN_NAME,
                "service": SERVICE_NAME,
                "abiVersion": dougong_abi::ABI_VERSION,
            }),
            METHOD_GET => {
                let agent = word(&request, "agent")?;
                json!({ "cwd": self.domain.get(agent)? })
            }
            METHOD_SET => {
                let agent = word(&request, "agent")?;
                let path = request
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!({ "cwd": self.domain.set(agent, path)? })
            }
            METHOD_ENSURE => {
                let agent = word(&request, "agent")?;
                json!({ "cwd": self.domain.ensure(agent)? })
            }
            METHOD_FOLD => json!({ "state": fold_log(&request) }),
            METHOD_CONTEXT_TEXT => {
                let agent = request.get("agent").and_then(Value::as_str);
                let text = self
                    .domain
                    .context_text(agent)
                    .map_err(|error| format!("context-text failure: {error}"))?;
                // The registry's context resolve reads the provider reply
                // as the bare text (the dsh context text fn returns a
                // string; the first real assemble caught the wrap).
                Value::String(text)
            }
            other => {
                return Err(format!(
                    "fr-working-directory provides no service method {other}"
                ));
            }
        };
        serde_json::to_vec(&reply).map_err(|error| format!("reply encode failed: {error}"))
    }
}

/// Reads one required string field (the wire request contract).
fn word<'a>(request: &'a Value, field: &'a str) -> Result<&'a str, String> {
    request
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("working-directory payload requires {field}"))
}

/// Projects the cold log onto the committed directory (the dsh
/// `workingDirectory` fold the relay calls; the request is the
/// persistence read shape).
fn fold_log(request: &Value) -> Value {
    let log = ColdLog {
        header: request.get("header").cloned().unwrap_or(Value::Null),
        inherited_event_count: request
            .get("inheritedEventCount")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        events: request
            .get("events")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    };
    match fold(&log) {
        ProjectionState::At(cwd) => Value::String(cwd),
        ProjectionState::Null => Value::Null,
    }
}

pub(crate) unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a cap-sized out-buffer and a writable
    // len slot (export-table contract).
    if unsafe { write_out(PLUGIN_NAME.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

pub(crate) unsafe extern "C" fn descriptor(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: same host buffer contract as plugin_name.
    if unsafe { write_out(DESCRIPTOR, out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

pub(crate) unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the host exports pointer is valid for the duration of
    // setup; the vtable is copied by value (v3 contract).
    let host = unsafe { *host };
    let outbound = Outbound::new(host);
    // The activation-time registrations, each fail-closed (a duplicate
    // or refusal declines the activation — the fr-tools-builtin
    // judgment).
    // The D31 grants (the registered endpoints are required on the
    // consumers' own vtables: the prompt registry calls the context
    // textProvider, the projection relay calls the fold; neither
    // consumer may declare an inject on this plugin without closing a
    // declaration cycle). Both targets are committed here: this plugin
    // injects them.
    for target in ["system-prompt", "session-projections"] {
        if let Err(error) = outbound.grant(target, SERVICE_NAME) {
            eprintln!("fr: working-directory: {error}");
            return std::ptr::null_mut();
        }
    }
    if let Err(error) = declare_change_event(&outbound) {
        eprintln!("fr: working-directory: {error}");
        return std::ptr::null_mut();
    }
    if let Err(error) = register_projection(&outbound) {
        eprintln!("fr: working-directory: {error}");
        return std::ptr::null_mut();
    }
    if let Err(error) = register_context(&outbound) {
        eprintln!("fr: working-directory: {error}");
        return std::ptr::null_mut();
    }
    let default_dir = match std::env::current_dir() {
        Ok(dir) => dir.to_string_lossy().into_owned(),
        Err(error) => {
            eprintln!("fr: working-directory: launch directory: {error}");
            return std::ptr::null_mut();
        }
    };
    let domain = WorkingDirectories::new(
        Arc::new(WireSessionLog::new(outbound)),
        Arc::new(WireDirectoryLeg::new(outbound)),
        Arc::new(WireAgentInject::new(outbound)),
        default_dir,
    );
    Box::into_raw(Box::new(ShellState::new(domain))).cast::<std::ffi::c_void>()
}

pub(crate) unsafe extern "C" fn call(
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
    let reply = match state.serve(method, payload) {
        Ok(reply) => reply,
        Err(text) => {
            let body = serde_json::json!({ "error": text }).to_string();
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

pub(crate) const unsafe extern "C" fn on_event(
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

pub(crate) const unsafe extern "C" fn on_next(
    _handle: *mut std::os::raw::c_void,
    _event: u32,
    _chain_status: i32,
    _data: *const u8,
    _len: usize,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    // No listeners are declared; the host never routes continuations
    // here.
    STATUS_FAILED
}

pub(crate) unsafe extern "C" fn teardown(handle: *mut std::os::raw::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was produced by `setup` and torn down exactly
    // once. No plugin-owned threads exist to join.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}

/// Test double for the host side of the ABI v6 `realm_spawn` entry (D33):
/// no directory test drives a realm activation, so the double accepts quietly.
#[cfg(test)]
pub(crate) const unsafe extern "C" fn quiet_realm_spawn(
    _host: *mut std::ffi::c_void,
    _realm: u64,
    _path: *const u8,
    _path_len: usize,
    _out_buf: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    dougong_abi::STATUS_OK
}

/// Test double for the host side of the ABI v6 `realm_retire` entry (D33):
/// no directory test drives a realm retirement, so the double accepts quietly.
#[cfg(test)]
pub(crate) const unsafe extern "C" fn quiet_realm_retire(
    _host: *mut std::ffi::c_void,
    _child: u64,
) -> i32 {
    dougong_abi::STATUS_OK
}

/// Test double for the host side of the ABI v6 `realm_require` entry (D33):
/// no directory test drives a realm call, so the double accepts quietly.
#[cfg(test)]
pub(crate) const unsafe extern "C" fn quiet_realm_require(
    _host: *mut std::ffi::c_void,
    _realm: u64,
    _service: *const u8,
    _service_len: usize,
    _method: u32,
    _in_data: *const u8,
    _in_len: usize,
    _out_buf: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    dougong_abi::STATUS_OK
}

#[cfg(test)]
mod tests {
    use super::{quiet_realm_require, quiet_realm_retire, quiet_realm_spawn};
    use crate::core::WorkingDirectories;
    use crate::core::test_support::{FakeInject, FakeLog, Scratch, StdLeg};
    use crate::outbound::{
        Outbound, context_registration_bytes, context_registration_spec,
        declared_event_types_bytes, projection_registration_bytes, projection_registration_spec,
    };
    use crate::wire::{
        METHOD_CONTEXT_TEXT, METHOD_DESCRIBE, METHOD_ENSURE, METHOD_FOLD, METHOD_GET, METHOD_SET,
    };
    use dougong_abi::HostExports;
    use serde_json::{Value, json};

    /// Decodes the descriptor literal.
    fn descriptor_value() -> Value {
        serde_json::from_slice(super::DESCRIPTOR).expect("descriptor JSON")
    }

    /// The shell over the std-fs test seams rooted at a scratch.
    fn state() -> (super::ShellState, Scratch) {
        let scratch = Scratch::new("shell");
        let domain = WorkingDirectories::new(
            std::sync::Arc::new(FakeLog::default()),
            std::sync::Arc::new(StdLeg),
            std::sync::Arc::new(FakeInject::default()),
            scratch.path("launch"),
        );
        (super::ShellState::new(domain), scratch)
    }

    /// The pinned registration payloads agree with the registration
    /// bytes (the byte builders wrap the specs).
    #[test]
    fn registration_bytes_round_trip_the_specs() {
        let projection: Value =
            serde_json::from_slice(&projection_registration_bytes()).expect("projection bytes");
        assert_eq!(projection, projection_registration_spec());
        let context: Value =
            serde_json::from_slice(&context_registration_bytes()).expect("context bytes");
        assert_eq!(context, context_registration_spec());
        let types: Value =
            serde_json::from_slice(&declared_event_types_bytes()).expect("declare bytes");
        assert_eq!(types["types"][0], json!("working-directory/change"));
    }

    /// The spec-bound face pin: the descriptor declares the plugin and
    /// service faces, the registration payloads point the fold and
    /// context endpoints at the plugin's own service, and the change
    /// vocabulary is declared (the builtin-tools contract's
    /// working-directory service-face clause).
    #[test]
    fn working_directory_service_pins_the_descriptor_and_registrations() {
        let descriptor = descriptor_value();
        assert_eq!(descriptor["name"], json!("fr-working-directory"));
        assert_eq!(descriptor["provide"], json!(["working-directory"]));
        assert_eq!(
            descriptor["inject"],
            json!([
                "session-persistence",
                "agent",
                "session-projections",
                "system-prompt"
            ])
        );
        assert_eq!(descriptor["listen"], json!({}));

        let projection = projection_registration_spec();
        assert_eq!(projection["key"], json!("workingDirectory"));
        assert_eq!(projection["stateVersion"], json!(1));
        assert_eq!(
            projection["fold"],
            json!({ "service": "working-directory", "method": 900 })
        );

        let context = context_registration_spec();
        assert_eq!(context["name"], json!("working-directory:current"));
        assert_eq!(context["order"], json!(100.0));
        assert_eq!(context["interpolate"], json!(false));
        assert_eq!(context["required"], json!(true));
        assert_eq!(
            context["text"]["textProvider"],
            json!({ "service": "working-directory", "method": 906 })
        );
    }

    /// The describe face echoes the small-provider metadata triple.
    #[test]
    fn describe_reports_the_plugin_metadata() {
        let (state, _scratch) = state();
        let reply: Value =
            serde_json::from_slice(&state.serve(METHOD_DESCRIBE, b"{}").expect("describe"))
                .expect("describe JSON");
        assert_eq!(reply["name"], json!("fr-working-directory"));
        assert_eq!(reply["service"], json!("working-directory"));
    }

    /// GET reads the committed directory (the default before any
    /// change).
    #[test]
    fn serve_get_reports_the_default_directory() {
        let (state, scratch) = state();
        let reply: Value = serde_json::from_slice(
            &state
                .serve(
                    METHOD_GET,
                    serde_json::to_vec(&json!({ "agent": "s1" }))
                        .unwrap()
                        .as_slice(),
                )
                .expect("get"),
        )
        .expect("get JSON");
        assert_eq!(reply["cwd"], json!(scratch.path("launch")));
    }

    /// SET commits the change and refuses the empty path with the dsh
    /// text (the error channel carries the message verbatim).
    #[test]
    fn serve_set_round_trips_and_refuses_with_the_dsh_text() {
        let (state, scratch) = state();
        let _ = scratch.make("target");
        let reply: Value = serde_json::from_slice(
            &state
                .serve(
                    METHOD_SET,
                    serde_json::to_vec(&json!({ "agent": "s1", "path": scratch.path("target") }))
                        .unwrap()
                        .as_slice(),
                )
                .expect("set"),
        )
        .expect("set JSON");
        assert_eq!(reply["cwd"], json!(scratch.path("target")));
        let error = state
            .serve(
                METHOD_SET,
                serde_json::to_vec(&json!({ "agent": "s1", "path": "" }))
                    .unwrap()
                    .as_slice(),
            )
            .unwrap_err();
        assert_eq!(error, "working-directory: cd must not be empty");
    }

    /// ENSURE passes the existing directory through.
    #[test]
    fn serve_ensure_passes_through_the_existing_directory() {
        let (state, scratch) = state();
        let _ = scratch.make("launch");
        let reply: Value = serde_json::from_slice(
            &state
                .serve(
                    METHOD_ENSURE,
                    serde_json::to_vec(&json!({ "agent": "s1" }))
                        .unwrap()
                        .as_slice(),
                )
                .expect("ensure"),
        )
        .expect("ensure JSON");
        assert_eq!(reply["cwd"], json!(scratch.path("launch")));
    }

    /// FOLD projects the relay's cold-log request onto the committed
    /// directory (string) or the empty state (null).
    #[test]
    fn serve_fold_projects_the_cold_log() {
        let (state, _scratch) = state();
        let request = json!({
            "header": { "cwd": "/repo" },
            "inheritedEventCount": 0,
            "events": [
                { "type": "working-directory/change", "data": { "cwd": "/next" } }
            ],
        });
        let reply: Value = serde_json::from_slice(
            &state
                .serve(
                    METHOD_FOLD,
                    serde_json::to_vec(&request).unwrap().as_slice(),
                )
                .expect("fold"),
        )
        .expect("fold JSON");
        assert_eq!(reply["state"], json!("/next"));
        let bare = json!({ "header": {}, "inheritedEventCount": 0, "events": [] });
        let reply: Value = serde_json::from_slice(
            &state
                .serve(METHOD_FOLD, serde_json::to_vec(&bare).unwrap().as_slice())
                .expect("bare fold"),
        )
        .expect("bare fold JSON");
        assert_eq!(reply["state"], Value::Null);
    }

    /// The context-text selector replies the bare string (the
    /// registry reads it as the text; the wrap caught the first real
    /// assemble).
    #[test]
    fn serve_context_text_returns_the_bare_string() {
        let (state, scratch) = state();
        let _ = scratch.make("launch");
        let reply: Value = serde_json::from_slice(
            &state
                .serve(
                    METHOD_CONTEXT_TEXT,
                    serde_json::to_vec(&json!({ "agent": "s1" }))
                        .unwrap()
                        .as_slice(),
                )
                .expect("context"),
        )
        .expect("context JSON");
        assert!(reply.is_string());
        assert_eq!(
            reply,
            json!(format!(
                "Current working directory: {}.",
                json!(scratch.path("launch"))
            ))
        );
        let empty: Value = serde_json::from_slice(
            &state
                .serve(METHOD_CONTEXT_TEXT, b"{}")
                .expect("empty context"),
        )
        .expect("empty context JSON");
        assert_eq!(empty, json!(""));
    }

    /// An unknown method refuses (the describe triple is the only
    /// unnamed face).
    #[test]
    fn serve_unknown_method_refuses() {
        let (state, _scratch) = state();
        let error = state.serve(4242, b"{}").unwrap_err();
        assert!(error.contains("4242"), "names the method: {error}");
    }

    /// The outbound refusal detail carries the `{"error"}` message
    /// body verbatim, through the host's prefixed require line too
    /// (the fr-approval carrier spelling).
    #[test]
    fn outbound_message_extracts_the_error_body() {
        assert_eq!(
            Outbound::message_of("{\"error\":\"working-directory: cd must not be empty\"}"),
            "working-directory: cd must not be empty"
        );
        assert_eq!(
            Outbound::message_of(
                "require working-directory.2 failed with host status 2: {\"error\":\"working-directory: cd must not be empty\"}"
            ),
            "working-directory: cd must not be empty"
        );
        assert_eq!(
            Outbound::message_of("plain transport text"),
            "plain transport text"
        );
    }

    /// The host-side vtable doubles: every arm fails loudly (the serve
    /// tests never touch the outbound faces; only the shapes bind).
    #[cfg(test)]
    unsafe extern "C" fn failing_require(
        _host: *mut std::ffi::c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        _payload: *const u8,
        _payload_len: usize,
        _out: *mut u8,
        _cap: usize,
        _out_len: *mut usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
    }

    #[cfg(test)]
    unsafe extern "C" fn failing_emit(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
    }

    #[cfg(test)]
    unsafe extern "C" fn failing_grant(
        _host: *mut std::ffi::c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
    }

    #[cfg(test)]
    unsafe extern "C" fn failing_dispatch(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _mode: u32,
        _payload: *const u8,
        _payload_len: usize,
        _out: *mut u8,
        _cap: usize,
        _out_len: *mut usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
    }

    /// The copied vtable shape stays constructible for the outbound
    /// wrapper (the fr-approval fake-host discipline; only the far
    /// side is scripted).
    #[test]
    fn outbound_wraps_the_copied_vtable() {
        let host = HostExports {
            host_handle: std::ptr::null_mut(),
            require: failing_require,
            emit: failing_emit,
            grant: failing_grant,
            dispatch: failing_dispatch,
            realm_spawn: quiet_realm_spawn,
            realm_retire: quiet_realm_retire,
            realm_require: quiet_realm_require,
        };
        let _outbound = Outbound::new(host);
    }
}
