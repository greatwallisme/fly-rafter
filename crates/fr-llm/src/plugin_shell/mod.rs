//! The C-ABI dylib plugin shell (PRD 4.5): fr-llm's artifact surface over
//! the dougong dynamic channel.
//!
//! Kernel-family edge: this module tree is the only `dougong-abi`
//! consumer; the rest of the crate stays a plain rlib that the provider
//! artifacts link for the shared behavior library (assembly settlement,
//! error taxonomy, retry policy, projection, attribution).
//!
//! The shell provides the `llm` service (PRD 4.5/:336 — fr-agent-loop
//! injects `llm`, and the host contains no loop logic, so the service
//! lives in this artifact). The B2 generation trio (Q84-B2, Q85-C) is
//! live: `prepare_call`/`begin`/`drain`/`abort` serve the shared
//! [`crate::generation`] hub, and the channel-side registration protocol
//! (the runtime-registry slice, 18b Q7A) routes provider keys through
//! the `channel` module's proxies onto the provider plugins' own
//! generation faces.

mod channel;
mod export;

use dougong_abi::{ABI_VERSION, HostExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out};
use serde_json::Value;

/// The provided-service name consumers inject (PRD :336 `inject llm`).
pub const SERVICE_NAME: &str = "llm";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-llm";

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 1;

/// Method selector: register one provider route (`{provider, service}`).
pub const METHOD_REGISTER_ROUTE: u32 = fr_wire::services::llm::METHOD_REGISTER_ROUTE;

/// Event id the descriptor's listen map binds to
/// `llm/provider-chunk-ready` (the channel proxies' pull-now wake).
const EVENT_PROVIDER_READY: u32 = 1;

pub use crate::generation::{
    METHOD_ABORT, METHOD_BEGIN, METHOD_DRAIN, METHOD_FILE_REQUEST_TEXT,
    METHOD_IMAGE_REQUEST_PRICING, METHOD_PREPARE_CALL, METHOD_RESOLVE_MODEL_INFO,
};

/// The descriptor the loader consumes: the `llm` service over the dynamic
/// channel, no injected dependencies, one listener for the provider
/// faces' pull-now notification. The version is sourced from crate
/// metadata so it cannot drift from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-llm\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"llm\"],\"inject\":[],\"listen\":{\"llm/provider-chunk-ready\":1}}"
)
.as_bytes();

/// The shell state: the generation service plus the copied emit vtable
/// its notification seam publishes `llm/chunk-ready` through, and the
/// channel registry the registered-route proxies share.
pub struct ShellState {
    service: crate::generation::GenerationService,
    registry: std::sync::Arc<channel::ChannelRegistry>,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("service", &self.service)
            .field("registry", &self.registry)
            .finish()
    }
}

impl ShellState {
    /// Assembles the state over one copied vtable (the emit seam; a null
    /// host keeps the quiet seam and a link-less channel registry).
    #[must_use]
    pub fn new(host: Option<crate::generation::EmitVtable>) -> Self {
        let notify = host.map_or_else(crate::generation::quiet_notify, |vtable| {
            vtable.notify_seam()
        });
        let registry = std::sync::Arc::new(channel::ChannelRegistry::new());
        // The vtable copy was taken by value at the `setup` boundary (the
        // sanctioned pattern, dylib-plugin.md); raw_host() hands the same
        // copy to the channel require link.
        if let Some(vtable) = host {
            let host = vtable.raw_host();
            registry.install_link(host);
        }
        Self {
            service: crate::generation::GenerationService::new(notify),
            registry,
        }
    }

    /// The hosted service (tests register sources and drive the trio).
    #[must_use]
    pub const fn service(&self) -> &crate::generation::GenerationService {
        &self.service
    }

    /// The shared channel registry (the proxy wake surface the tests
    /// drive through the fake host's emit sink).
    #[must_use]
    #[cfg(test)]
    fn registry(&self) -> std::sync::Arc<channel::ChannelRegistry> {
        std::sync::Arc::clone(&self.registry)
    }

    /// Registers one provider route onto the shared registry: the
    /// provider key resolves to a channel source over the named face
    /// service (duplicate keys fail closed).
    ///
    /// # Errors
    ///
    /// A text failure for a malformed payload or a duplicate provider
    /// key.
    pub fn register_route(&self, provider: &str, service: &str) -> Result<(), String> {
        if provider.is_empty() || service.is_empty() {
            return Err(
                "register-route payload requires non-empty provider and service".to_owned(),
            );
        }
        let source = std::sync::Arc::new(channel::ChannelSource::new(
            service.to_owned(),
            std::sync::Arc::clone(&self.registry),
        ));
        self.service.register_provider_route(provider, source)?;
        Ok(())
    }
}

/// One failed method call: `Llm` carries the taxonomy document the
/// shell replies as the `STATUS_FAILED` body verbatim; `Text` failures
/// wrap as `{"error": ...}` — the two reply shapes the loop's client
/// decodes.
#[derive(Debug)]
pub enum ShellFailure {
    /// The `{"llmError": ...}` document (already JSON bytes).
    Llm(Vec<u8>),
    /// A fabric protocol failure.
    Text(String),
}

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
                "service": SERVICE_NAME,
                "abiVersion": ABI_VERSION,
            }))
            .unwrap_or_default())
        }
        METHOD_REGISTER_ROUTE => {
            let request: Value = serde_json::from_slice(payload)
                .map_err(|error| ShellFailure::Text(format!("register-route payload: {error}")))?;
            let provider = request
                .get("provider")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ShellFailure::Text("register-route payload requires provider".to_owned())
                })?;
            let service = request
                .get("service")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ShellFailure::Text("register-route payload requires service".to_owned())
                })?;
            state
                .register_route(provider, service)
                .map_err(ShellFailure::Text)?;
            Ok(serde_json::to_vec(&serde_json::json!({})).unwrap_or_default())
        }
        METHOD_PREPARE_CALL
        | METHOD_BEGIN
        | METHOD_DRAIN
        | METHOD_ABORT
        | METHOD_RESOLVE_MODEL_INFO
        | METHOD_IMAGE_REQUEST_PRICING
        | METHOD_FILE_REQUEST_TEXT => {
            match crate::generation::serve(&state.service, method, payload) {
                Ok(reply) => Ok(reply),
                Err(crate::generation::ServiceError::Llm(failure)) => {
                    // The failure IS the reply body (STATUS_FAILED).
                    match serde_json::to_vec(&serde_json::json!({ "llmError": failure.failure() }))
                    {
                        Ok(body) => Err(ShellFailure::Llm(body)),
                        Err(error) => Err(ShellFailure::Text(error.to_string())),
                    }
                }
                Err(crate::generation::ServiceError::Text(text)) => Err(ShellFailure::Text(text)),
            }
        }
        _ => Err(ShellFailure::Text(format!(
            "unknown fr-llm method {method}"
        ))),
    }
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
        Some(crate::generation::EmitVtable::new(unsafe { *host }))
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
    if handle.is_null() || event != EVENT_PROVIDER_READY {
        return STATUS_FAILED;
    }
    // SAFETY: the handle was produced by `setup` and is valid until the
    // matching `teardown`, which the host never overlaps with an event.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    // Fast-path invariant: parse the payload and raise the wake bit; no
    // `require`, no blocking (decision 46).
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    handle_provider_ready(state, payload);
    STATUS_OK
}

/// The provider pull-now wake handler: parse `{sessionId, generationId}`
/// and raise the matching proxy's ready bit. Unknown ids drop on the
/// floor (the notification is advisory; the proxy's unconditional first
/// drain covers everything buffered before registration).
fn handle_provider_ready(state: &ShellState, payload: &[u8]) {
    let Ok(document) = serde_json::from_slice::<Value>(payload) else {
        return;
    };
    let Some(generation_id) = document.get("generationId").and_then(Value::as_str) else {
        return;
    };
    state.registry.mark_ready(generation_id);
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
    // Teardown order for the channel proxies: cancel the hub's signals
    // first (the abort relays fire, the proxies converge and exit),
    // join the proxies, then shut the hub down (its workers all see a
    // finished stream by now). The vtable lifetime ends after the last
    // join (the dylib thread discipline).
    // SAFETY: the handle was produced by `setup` and torn down exactly once.
    let state = unsafe { Box::from_raw(handle.cast::<ShellState>()) };
    state.service.cancel_all();
    state.registry.join_workers();
    state.service.shutdown();
}

#[cfg(test)]
mod tests {
    use super::{
        METHOD_BEGIN, METHOD_DESCRIBE, METHOD_DRAIN, METHOD_FILE_REQUEST_TEXT,
        METHOD_REGISTER_ROUTE, PLUGIN_NAME, SERVICE_NAME, ShellState, dispatch,
    };
    use dougong_abi::{
        HostExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, STATUS_UNDECLARED, write_out,
    };
    use fr_wire::StreamChunk;
    use futures_core::Stream;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    fn quiet_state() -> ShellState {
        ShellState::new(None)
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let state = quiet_state();
        let reply = dispatch(&state, METHOD_DESCRIBE, b"{}").expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], PLUGIN_NAME);
        assert_eq!(reply["service"], SERVICE_NAME);
        assert_eq!(reply["abiVersion"], dougong_abi::ABI_VERSION);
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        let state = quiet_state();
        let failure = dispatch(&state, 99, b"{}").expect_err("unknown method");
        assert!(
            matches!(failure, super::ShellFailure::Text(ref text) if text.contains("unknown fr-llm method 99"))
        );
    }

    /// The trio reaches the hub through the shell dispatch and the
    /// llm-shaped failure carries the taxonomy document (the shape the
    /// loop's client decodes).
    #[test]
    fn begin_without_a_route_replies_the_llm_error_document() {
        let state = quiet_state();
        let begin = br#"{"request":{"provider":"missing","model":"m","messages":[]}}"#;
        let body = match dispatch(&state, METHOD_BEGIN, begin).expect_err("llm failure") {
            super::ShellFailure::Llm(body) => body,
            super::ShellFailure::Text(_) => panic!("expected the llm error document"),
        };
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(parsed["llmError"]["code"], "NO_ADAPTER");
        assert_eq!(
            parsed["llmError"]["message"],
            "no adapter registered for provider \"missing\""
        );
    }

    #[test]
    fn descriptor_declares_the_llm_service() {
        // Parses the shipped `DESCRIPTOR` constant itself, so any drift (for
        // example a stale embedded version) fails this test.
        let parsed: serde_json::Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"][0], "llm");
        assert_eq!(parsed["inject"], serde_json::json!([]));
        assert_eq!(
            parsed["listen"],
            serde_json::json!({"llm/provider-chunk-ready": 1})
        );
    }

    #[test]
    fn method_constants_match_the_fr_wire_table() {
        use fr_wire::services::llm as wire;
        assert_eq!(super::METHOD_DESCRIBE, wire::METHOD_DESCRIBE);
        assert_eq!(super::METHOD_PREPARE_CALL, wire::METHOD_PREPARE_CALL);
        assert_eq!(super::METHOD_BEGIN, wire::METHOD_BEGIN);
        assert_eq!(super::METHOD_DRAIN, wire::METHOD_DRAIN);
        assert_eq!(super::METHOD_ABORT, wire::METHOD_ABORT);
        assert_eq!(
            super::METHOD_RESOLVE_MODEL_INFO,
            wire::METHOD_RESOLVE_MODEL_INFO
        );
        assert_eq!(
            super::METHOD_IMAGE_REQUEST_PRICING,
            wire::METHOD_IMAGE_REQUEST_PRICING
        );
        assert_eq!(
            super::METHOD_FILE_REQUEST_TEXT,
            wire::METHOD_FILE_REQUEST_TEXT
        );
        assert_eq!(super::METHOD_REGISTER_ROUTE, wire::METHOD_REGISTER_ROUTE);
    }

    /// The meter's file-text face routes through the shell and answers
    /// the no-path handle (the dispatch projection is not wired yet).
    #[test]
    fn file_request_text_answers_the_no_path_handle() {
        use fr_wire::content::FileAttachmentRef;
        let state = quiet_state();
        let reference = FileAttachmentRef {
            attachment_id: fr_wire::ids::AttachmentId::new("6f0a3f6c8f6c4b9fb5d1d0a3f6c8f6c4"),
            name: "report.txt".to_owned(),
            bytes: 128,
        };
        let payload = serde_json::json!({ "ref": reference }).to_string();
        let reply =
            dispatch(&state, METHOD_FILE_REQUEST_TEXT, payload.as_bytes()).expect("file text");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(
            reply["text"],
            crate::projection::file_handle_text(&reference, None)
        );
        assert!(
            reply["text"]
                .as_str()
                .is_some_and(|text| text.contains("cannot access a readable path")),
            "the no-path arm names the limitation: {:?}",
            reply["text"]
        );
    }

    // --- the runtime-registry full-loop doubles (18b Q7A) ---

    /// The fake host side of the full-loop test: requires route to a local
    /// face service, emits loop back into the shell's wake registry (the
    /// kernel bus the real composition provides).
    struct LoopHost {
        face: std::sync::OnceLock<std::sync::Arc<crate::generation::GenerationService>>,
        registry: std::sync::OnceLock<std::sync::Arc<super::channel::ChannelRegistry>>,
    }

    impl LoopHost {
        fn new() -> Self {
            Self {
                face: std::sync::OnceLock::new(),
                registry: std::sync::OnceLock::new(),
            }
        }

        fn handle(&self) -> *mut std::os::raw::c_void {
            std::ptr::from_ref(self)
                .cast::<std::ffi::c_void>()
                .cast_mut()
        }
    }

    /// The one leaked loop host every test and every vtable copy shares
    /// (a per-call leak would split the face from the vtable).
    fn loop_host() -> &'static LoopHost {
        static HOST: std::sync::OnceLock<&'static LoopHost> = std::sync::OnceLock::new();
        HOST.get_or_init(|| Box::leak(Box::new(LoopHost::new())))
    }

    /// Builds the vtable over the leaked loop host.
    fn loop_exports() -> HostExports {
        let host = loop_host();
        HostExports {
            host_handle: host.handle(),
            require: loop_require,
            grant: failed_grant,
            emit: loop_emit,
            dispatch: failed_dispatch,
        }
    }

    /// The require double: one face service named `llm-deepseek` served by
    /// the shared `serve` dispatch.
    ///
    /// # Safety
    ///
    /// The fake vtable's own contract — the handle is the leaked
    /// `LoopHost`, buffers are valid for their lengths, and the out buffer
    /// is caller-owned per the export-table contract.
    unsafe extern "C" fn loop_require(
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
        // SAFETY: the handle is the leaked `LoopHost` the test set.
        let host = unsafe { &*host.cast::<LoopHost>() };
        // SAFETY: the name buffer is valid for `service_len` per the
        // vtable contract.
        let service = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(service, service_len)).into_owned()
        };
        let payload: &[u8] = if in_len == 0 {
            &[]
        } else {
            // SAFETY: `in_len` is nonzero, the pointer valid for it.
            unsafe { std::slice::from_raw_parts(in_data, in_len) }
        };
        // Only the face service routes; every other name is undeclared
        // (the boundary the registration contract establishes).
        if service != "llm-deepseek" {
            return STATUS_UNDECLARED;
        }
        let Some(face) = host.face.get() else {
            return STATUS_FAILED;
        };
        let body = match crate::generation::serve(face, method, payload) {
            Ok(reply) => reply,
            Err(crate::generation::ServiceError::Llm(failure)) => serde_json::json!({
                "llmError": failure.failure()
            })
            .to_string()
            .into_bytes(),
            Err(crate::generation::ServiceError::Text(text)) => {
                serde_json::json!({ "error": text })
                    .to_string()
                    .into_bytes()
            }
        };
        // SAFETY: caller-owned out-buffer contract.
        if unsafe { write_out(&body, out, cap, out_len) } {
            STATUS_OK
        } else {
            STATUS_CAPACITY
        }
    }

    /// The emit double: the face's chunk-ready payload raises the wake bit
    /// directly (the kernel bus the real composition provides in between).
    ///
    /// # Safety
    ///
    /// Same fake-vtable contract as `loop_require`.
    unsafe extern "C" fn loop_emit(
        host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        payload: *const u8,
        payload_len: usize,
    ) -> i32 {
        // SAFETY: the handle is the leaked `LoopHost` the test set.
        let host = unsafe { &*host.cast::<LoopHost>() };
        let Some(registry) = host.registry.get() else {
            return STATUS_FAILED;
        };
        let payload: &[u8] = if payload_len == 0 {
            &[]
        } else {
            // SAFETY: `payload_len` is nonzero, the pointer valid for it.
            unsafe { std::slice::from_raw_parts(payload, payload_len) }
        };
        let Ok(document) = serde_json::from_slice::<serde_json::Value>(payload) else {
            return STATUS_FAILED;
        };
        let Some(generation_id) = document
            .get("generationId")
            .and_then(serde_json::Value::as_str)
        else {
            return STATUS_FAILED;
        };
        registry.mark_ready(generation_id);
        STATUS_OK
    }

    unsafe extern "C" fn failed_grant(
        _host: *mut std::os::raw::c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        STATUS_FAILED
    }

    unsafe extern "C" fn failed_dispatch(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _mode: u32,
        _payload: *const u8,
        _payload_len: usize,
        _out_buf: *mut u8,
        _cap: usize,
        _out_len: *mut usize,
    ) -> i32 {
        STATUS_FAILED
    }

    /// The scripted face source: one text chunk then the stop finish (the
    /// generation-hub test judgment's stream face).
    struct FaceScriptSource;

    impl crate::generation::GenerationSource for FaceScriptSource {
        fn prepare(
            &self,
            config: &crate::adapter::LlmCallConfig,
        ) -> Result<
            (
                crate::generation::PreparedRecord,
                crate::adapter::LlmCallConfig,
            ),
            crate::error::LlmError,
        > {
            let model = crate::adapter::ResolvedModelInfo {
                provider: config.provider.clone(),
                id: config.model.clone(),
                ..crate::adapter::ResolvedModelInfo::default()
            };
            Ok((
                crate::generation::PreparedRecord {
                    model,
                    adapter_defaults: None,
                    retry_policy: None,
                },
                config.clone(),
            ))
        }

        fn stream(
            &self,
            _options: crate::adapter::GenerateOptions,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            let chunks = vec![
                StreamChunk::Known(fr_wire::stream::KnownStreamChunk::TextDelta {
                    index: 0,
                    text: "hello".to_owned(),
                }),
                StreamChunk::Known(fr_wire::stream::KnownStreamChunk::Finish {
                    reason: fr_wire::call_config::FinishReason::Known(
                        fr_wire::call_config::FinishReasonKind::Stop,
                    ),
                    replay_state: None,
                }),
            ];
            Box::pin(StraightStream {
                chunks: VecDeque::from(chunks),
            })
        }
    }

    /// A stream that yields its chunks immediately.
    struct StraightStream {
        chunks: VecDeque<StreamChunk>,
    }

    impl Stream for StraightStream {
        type Item = StreamChunk;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
            Poll::Ready(self.chunks.pop_front())
        }
    }

    /// The full loop (the spec scenario `registered_route_streams_
    /// through_the_channel_proxy`): registration, begin through the
    /// registered route, and the scripted chunks crossing the channel
    /// proxy in order with the terminal settlement.
    #[test]
    fn registered_route_streams_through_the_channel_proxy() {
        let exports = loop_exports();
        let face = std::sync::Arc::new(crate::generation::GenerationService::new(
            crate::generation::EmitVtable::new(exports).notify_seam(),
        ));
        face.register_source("deepseek-official", std::sync::Arc::new(FaceScriptSource));
        loop_host()
            .face
            .set(std::sync::Arc::clone(&face))
            .expect("the face installs once");

        let state: &'static ShellState = Box::leak(Box::new(ShellState::new(Some(
            crate::generation::EmitVtable::new(exports),
        ))));
        loop_host()
            .registry
            .set(state.registry())
            .expect("the registry installs once");

        // The route registers once and duplicate keys fail closed.
        dispatch(
            state,
            METHOD_REGISTER_ROUTE,
            br#"{"provider":"deepseek-official","service":"llm-deepseek"}"#,
        )
        .expect("register the route");
        let duplicate = dispatch(
            state,
            METHOD_REGISTER_ROUTE,
            br#"{"provider":"deepseek-official","service":"llm-deepseek"}"#,
        )
        .expect_err("duplicate route");
        assert!(
            matches!(duplicate, super::ShellFailure::Text(ref text) if text.contains("already has a registered source"))
        );

        // Begin through the registered route.
        let reply = dispatch(
            state,
            METHOD_BEGIN,
            br#"{"request":{"provider":"deepseek-official","model":"m","messages":[]}}"#,
        )
        .expect("begin through the channel");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("begin json");
        let generation_id = reply["generationId"].as_str().expect("generation id");

        // Drain until the terminal lands (bounded; the proxy converges on
        // the face notifications the fake emit relays).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut chunks: Vec<serde_json::Value> = Vec::new();
        let mut done = false;
        while !done {
            assert!(
                std::time::Instant::now() < deadline,
                "the channel proxy did not settle within the test budget"
            );
            let request = serde_json::json!({ "generationId": generation_id }).to_string();
            let reply = dispatch(state, METHOD_DRAIN, request.as_bytes()).expect("drain");
            let reply: serde_json::Value = serde_json::from_slice(&reply).expect("drain json");
            chunks.extend(reply["chunks"].as_array().cloned().unwrap_or_default());
            done = reply["done"].as_bool().unwrap_or(false);
            if !done {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        let texts: Vec<&str> = chunks
            .iter()
            .filter_map(|chunk| chunk.get("text").and_then(serde_json::Value::as_str))
            .collect();
        assert_eq!(texts, vec!["hello"], "chunks crossed in order: {chunks:?}");
        assert!(
            chunks.iter().any(
                |chunk| chunk.get("type").and_then(serde_json::Value::as_str) == Some("finish")
            ),
            "the terminal settlement crossed the channel"
        );

        // The proxy workers all exited after forwarding the terminal.
        state.registry().join_workers();
    }

    /// An unknown provider key still fails closed with the taxonomy
    /// document (the registration arm does not open a side door).
    #[test]
    fn unregistered_provider_still_fails_closed() {
        let exports = loop_exports();
        let state: &'static ShellState = Box::leak(Box::new(ShellState::new(Some(
            crate::generation::EmitVtable::new(exports),
        ))));
        // The registry is not needed here: no route means the begin fails
        // before any channel traffic.
        let begin = br#"{"request":{"provider":"missing","model":"m","messages":[]}}"#;
        let body = match dispatch(state, METHOD_BEGIN, begin).expect_err("no route") {
            super::ShellFailure::Llm(body) => body,
            super::ShellFailure::Text(_) => panic!("expected the llm error document"),
        };
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(parsed["llmError"]["code"], "NO_ADAPTER");
    }
}

// Rust guideline compliant 2026-10-25
