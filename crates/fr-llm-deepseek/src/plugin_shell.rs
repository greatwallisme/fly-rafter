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
/// service injected over the dynamic channel plus the two providers the
/// access resolver reads (the dsh `resolveAttachments` pair), no
/// listeners in this slice. The version is sourced from crate metadata
/// so it cannot drift from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-llm-deepseek\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[],\"inject\":[\"llm\",\"attachments\",\"fs\"],\"listen\":{}}"
)
.as_bytes();

/// Upper bound for one access-resolution round trip (the shared ceiling).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// One copied host vtable with the synchronous require the access
/// resolver needs (callable from any adapter thread until `teardown`
/// returns; the same copied-vtable contract as the emit seam).
#[derive(Debug, Clone, Copy)]
struct AccessLink {
    host: HostExports,
}

// SAFETY: the ABI vtable contract states every entry is callable from any
// plugin thread, concurrently, with host-side panic containment; the copy
// stays valid until this activation's `teardown` returns.
unsafe impl Send for AccessLink {}
// SAFETY: sharing one copy only issues concurrent vtable calls, which the
// same contract explicitly allows.
unsafe impl Sync for AccessLink {}

impl AccessLink {
    /// Performs one synchronous cross-plugin call (growable buffer
    /// protocol; `STATUS_CAPACITY` doubles up to the shared ceiling).
    fn require(&self, service: &str, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: the copied vtable is live until this activation's
            // `teardown` returns; buffers are caller-owned per the
            // export-table contract.
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

    /// Resolves the provider path of one durable image reference through
    /// the `attachments` face; `None` when the backend declines (the dsh
    /// `imageHostPath` `undefined`).
    fn image_host_path(&self, reference: &fr_wire::ImageAttachmentRef) -> Option<String> {
        let payload = serde_json::json!({ "ref": reference });
        let bytes = serde_json::to_vec(&payload).ok()?;
        let reply = self
            .require(
                fr_wire::services::attachments::SERVICE_NAME,
                fr_wire::services::attachments::METHOD_IMAGE_HOST_PATH,
                &bytes,
            )
            .ok()?;
        let parsed: serde_json::Value = serde_json::from_slice(&reply).ok()?;
        parsed.get("path")?.as_str().map(str::to_owned)
    }

    /// Maps one host path into the execution world through the `fs`
    /// face (the dsh `processPathFromHostPath`); `None` when unresolvable.
    fn process_path_from_host_path(&self, host_path: &str) -> Option<String> {
        let payload = serde_json::json!({ "hostPath": host_path });
        let bytes = serde_json::to_vec(&payload).ok()?;
        let reply = self
            .require(
                fr_wire::services::fs::SERVICE_NAME,
                fr_wire::services::fs::METHOD_PROCESS_PATH_FROM_HOST_PATH,
                &bytes,
            )
            .ok()?;
        let parsed: serde_json::Value = serde_json::from_slice(&reply).ok()?;
        parsed.get("path")?.as_str().map(str::to_owned)
    }
}

/// Builds the wire-backed execution-world access resolver (the dsh
/// adapter `imageAccess` closure: `resolveAttachments` + the runtime's
/// `resolveImageAccess`).
///
/// The runtime-registry slice installs the returned callback on the
/// adapter's [`crate::types::Dependencies`].
///
/// A failed lookup answers `None` — the dsh closure's `undefined` when a
/// provider is absent — keeping the no-path arm of the handle text and
/// the pricing text. The store-side invalid-reference refusal also lands
/// here (the callback type has no error channel), the one bounded
/// divergence from the dsh throw: the request still fails later at the
/// attachment read, with a different message.
#[must_use]
pub fn wire_image_access(host: HostExports) -> crate::types::ImageAccessCallback {
    let link = AccessLink { host };
    std::sync::Arc::new(move |reference: &fr_wire::ImageAttachmentRef| {
        let host_path = link.image_host_path(reference)?;
        let readonly_path = link.process_path_from_host_path(&host_path)?;
        Some(fr_llm::projection::ImageAttachmentAccess { readonly_path })
    })
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
    use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_OK, write_out};
    use std::sync::Mutex;

    /// A fake host whose require face routes scripted replies per service
    /// (the access resolver's two faces answer differently).
    struct AccessHost {
        replies: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    }

    impl AccessHost {
        fn handle(&self) -> *mut std::os::raw::c_void {
            std::ptr::from_ref(self)
                .cast::<std::ffi::c_void>()
                .cast_mut()
        }
    }

    /// Builds an access host answering every require for `service` with
    /// the given JSON body.
    fn access_host(attachments_reply: String, fs_reply: String) -> HostExports {
        let mut replies = std::collections::HashMap::new();
        replies.insert(
            fr_wire::services::attachments::SERVICE_NAME.to_owned(),
            attachments_reply.into_bytes(),
        );
        replies.insert(
            fr_wire::services::fs::SERVICE_NAME.to_owned(),
            fs_reply.into_bytes(),
        );
        let host = Box::leak(Box::new(AccessHost {
            replies: Mutex::new(replies),
        }));
        HostExports {
            host_handle: host.handle(),
            require: routed_require,
            grant: failed_grant,
            emit: failed_emit,
            dispatch: failed_dispatch,
        }
    }

    /// A host whose every require fails (the absent-provider degrade).
    fn dead_host() -> HostExports {
        let host = Box::leak(Box::new(AccessHost {
            replies: Mutex::new(std::collections::HashMap::new()),
        }));
        HostExports {
            host_handle: host.handle(),
            require: routed_require,
            grant: failed_grant,
            emit: failed_emit,
            dispatch: failed_dispatch,
        }
    }

    /// The require double: routes to the scripted per-service reply with
    /// the buffer protocol.
    ///
    /// # Safety
    ///
    /// The fake vtable's own contract — the handle is the leaked
    /// `AccessHost` the test set, name buffers are valid for their
    /// lengths, and the out buffer is caller-owned per the export-table
    /// contract.
    unsafe extern "C" fn routed_require(
        host: *mut std::os::raw::c_void,
        service: *const u8,
        service_len: usize,
        _method: u32,
        _in_data: *const u8,
        _in_len: usize,
        out: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the handle is the leaked `AccessHost` the test set.
        let host = unsafe { &*host.cast::<AccessHost>() };
        // SAFETY: the name buffer is valid for `service_len` per the
        // vtable contract.
        let service = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(service, service_len)).into_owned()
        };
        let reply = host
            .replies
            .lock()
            .expect("lock")
            .get(&service)
            .cloned()
            .unwrap_or_default();
        // SAFETY: caller-owned out-buffer contract.
        if unsafe { write_out(&reply, out, cap, out_len) } {
            STATUS_OK
        } else {
            STATUS_CAPACITY
        }
    }

    unsafe extern "C" fn failed_grant(
        _host: *mut std::os::raw::c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
    }

    unsafe extern "C" fn failed_emit(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
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
        dougong_abi::STATUS_FAILED
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let state = ShellState::new(None);
        let reply = dispatch(&state, METHOD_DESCRIBE, b"{}").expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], PLUGIN_NAME);
        assert_eq!(reply["injects"], "llm");
        assert_eq!(reply["abiVersion"], dougong_abi::ABI_VERSION);
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
        assert_eq!(
            parsed["inject"],
            serde_json::json!(["llm", "attachments", "fs"])
        );
        assert_eq!(parsed["listen"], serde_json::json!({}));
    }

    /// The access resolver walks attachments → fs and answers `None` on
    /// each absent arm (the dsh closure's `undefined` pair).
    #[test]
    fn wire_image_access_resolves_through_the_two_faces() {
        let reference: fr_wire::ImageAttachmentRef = serde_json::from_value(serde_json::json!({
            "attachmentId": "sha256:abc",
            "mediaType": "image/png",
            "bytes": 128,
            "width": 2,
            "height": 2,
        }))
        .expect("reference");
        let host = super::tests::access_host(
            // The attachments face answers the stored host path.
            serde_json::json!({ "path": "/host/objects/ab/abc.png" }).to_string(),
            // The fs face maps it into the execution world.
            serde_json::json!({ "path": "/exec/objects/ab/abc.png" }).to_string(),
        );
        let access = super::wire_image_access(host);
        let resolved = access(&reference).expect("resolves");
        assert_eq!(resolved.readonly_path, "/exec/objects/ab/abc.png");

        // Either face answering null keeps the no-path arm.
        let host = super::tests::access_host(
            serde_json::json!({ "path": null }).to_string(),
            serde_json::json!({ "path": null }).to_string(),
        );
        let access = super::wire_image_access(host);
        assert!(access(&reference).is_none(), "null path is the absent arm");

        // A transport failure keeps the no-path arm too (the vtable-free
        // link fails every require).
        let access = super::wire_image_access(super::tests::dead_host());
        assert!(access(&reference).is_none(), "transport failure is absent");
    }
}

// Rust guideline compliant 2026-10-26
