//! The C-ABI dylib plugin shell (PRD 4.5): fr-token-meter's artifact
//! surface over the dougong dynamic channel.
//!
//! The shell provides the `token-meter` service (PRD 4.5 row; PRD 19.2
//! entry 37 erratum) and wires itself into the assembly at `setup`: it
//! grants the session-projections relay the fold-call right over its
//! three projection-unit endpoints (Q110′A′, the permission-presets
//! precedent), then registers `tokenUsage` (stateVersion 2),
//! `contextPressure` (stateVersion 5), and `contextBreakdown`
//! (stateVersion 6). The `llm` inject is the fr declared-dependency
//! shape of the dsh soft get (see the crate docs).

use std::collections::{HashMap, VecDeque};

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};
use fr_wire::event::SessionEvent;
use serde_json::{Value, json};

use crate::meter::{ReplayState, RouteResolvers, fold_event, measure};

/// The provided-service name consumers inject.
pub const SERVICE_NAME: &str = fr_wire::services::token_meter::SERVICE_NAME;

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-token-meter";

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = fr_wire::services::token_meter::METHOD_DESCRIBE;
/// Method selector: measure one session's request pressure and surface.
pub const METHOD_MEASURE: u32 = fr_wire::services::token_meter::METHOD_MEASURE;
/// Method selector: price one message under the fixed estimator.
pub const METHOD_ESTIMATE_MESSAGE: u32 = fr_wire::services::token_meter::METHOD_ESTIMATE_MESSAGE;
/// Method selector: fold the `tokenUsage` unit (stateVersion 2).
pub const METHOD_FOLD_TOKEN_USAGE: u32 = fr_wire::services::token_meter::METHOD_FOLD_TOKEN_USAGE;
/// Method selector: fold the `contextPressure` unit (stateVersion 5).
pub const METHOD_FOLD_CONTEXT_PRESSURE: u32 =
    fr_wire::services::token_meter::METHOD_FOLD_CONTEXT_PRESSURE;
/// Method selector: fold the `contextBreakdown` unit (stateVersion 6).
pub const METHOD_FOLD_CONTEXT_BREAKDOWN: u32 =
    fr_wire::services::token_meter::METHOD_FOLD_CONTEXT_BREAKDOWN;

/// Upper bound for one outbound round trip on the growable buffer
/// protocol (the dougong host documents a 16 MiB ceiling).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// The descriptor the loader consumes: provides `token-meter`, injects
/// the three consumed services, and listens for nothing (the dsh eager
/// `session/event` sync has no fr kernel event; the fold catches up at
/// measure time — behavior-identical, see the crate docs).
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-token-meter\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"token-meter\"],\"inject\":[\"session-persistence\",",
    "\"session-projections\",\"llm\"],\"listen\":{}}"
)
.as_bytes();

/// The per-session replay-state cache bound (the dsh `WeakMap`+GC
/// counterpart: eviction only forces a from-scratch re-replay).
const SESSION_CACHE_CAPACITY: usize = 128;

/// One copied host vtable with the outbound arms this shell uses:
/// require (every wired seam) and grant (the relay's fold-call right).
#[derive(Debug, Clone, Copy)]
pub struct HostLink {
    host: HostExports,
}

// SAFETY: the ABI vtable contract allows require/grant from any plugin
// thread, concurrently; the copy stays valid until this activation's
// `teardown` returns, and this shell never outlives its activation.
unsafe impl Send for HostLink {}

// SAFETY: sharing one copy only issues concurrent vtable calls, which the
// same contract explicitly allows.
unsafe impl Sync for HostLink {}

impl HostLink {
    /// Wraps a vtable copied at `setup`.
    const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Grants `target`'s provider the right to require `granted` (D31;
    /// fail-closed statuses surface to the caller).
    fn grant(&self, target: &str, granted: &str) -> Result<(), String> {
        // SAFETY: the copied vtable is live until this activation's
        // `teardown` returns; both name buffers live for the call.
        let status = unsafe {
            (self.host.grant)(
                self.host.host_handle,
                target.as_ptr(),
                target.len(),
                granted.as_ptr(),
                granted.len(),
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
    fn require(&self, service: &str, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: same vtable lifetime contract as `grant`; buffers
            // are caller-owned per the export-table contract.
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
}

/// The per-session replay-state cache (the dsh `WeakMap` keyed by live
/// session; keyed by id here, bounded, with recomputation on eviction).
#[derive(Debug, Default)]
struct LruSessions {
    map: HashMap<String, ReplayState>,
    order: VecDeque<String>,
}

impl LruSessions {
    /// The entry for one session, refreshed to most-recently-used and
    /// created when absent; the oldest entry evicts past the bound.
    fn entry(&mut self, id: &str) -> &mut ReplayState {
        if self.map.contains_key(id) {
            if let Some(position) = self.order.iter().position(|seen| seen == id) {
                self.order.remove(position);
            }
        } else if self.order.len() >= SESSION_CACHE_CAPACITY
            && let Some(evicted) = self.order.pop_front()
        {
            self.map.remove(&evicted);
        }
        self.order.push_back(id.to_owned());
        self.map.entry(id.to_owned()).or_default()
    }
}

/// The shell state: the replay cache and the copied vtable every seam
/// rides.
pub struct ShellState {
    link: HostLink,
    sessions: LruSessions,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("sessions", &self.sessions)
            .finish_non_exhaustive()
    }
}

/// The unit registrations (key, stateVersion, fold method) the setup
/// issues, in dsh constructor order.
const REGISTRATIONS: [(&str, u64, u32); 3] = [
    (
        "tokenUsage",
        2,
        fr_wire::services::token_meter::METHOD_FOLD_TOKEN_USAGE,
    ),
    (
        "contextPressure",
        5,
        fr_wire::services::token_meter::METHOD_FOLD_CONTEXT_PRESSURE,
    ),
    (
        "contextBreakdown",
        6,
        fr_wire::services::token_meter::METHOD_FOLD_CONTEXT_BREAKDOWN,
    ),
];

impl ShellState {
    /// Assembles the state over one copied vtable, wiring the whole
    /// registration sequence; `None` declines the activation
    /// fail-closed (any grant or registration refusal leaves the
    /// assembly without a meter rather than a half-wired one).
    #[must_use]
    pub fn try_new(link: Option<HostLink>) -> Option<Self> {
        let link = link?;
        link.grant(
            fr_wire::services::session_projections::SERVICE_NAME,
            SERVICE_NAME,
        )
        .ok()?;
        for (key, state_version, method) in REGISTRATIONS {
            let registration = json!({
                "key": key,
                "stateVersion": state_version,
                "fold": { "service": SERVICE_NAME, "method": method },
            });
            let bytes = serde_json::to_vec(&registration).ok()?;
            link.require(
                fr_wire::services::session_projections::SERVICE_NAME,
                fr_wire::services::session_projections::METHOD_REGISTER,
                &bytes,
            )
            .ok()?;
        }
        Some(Self {
            link,
            sessions: LruSessions::default(),
        })
    }
}

impl ShellState {
    /// Serves one method invocation against the shell state (the
    /// `call` export's pure face, so the surface stays testable).
    ///
    /// # Errors
    ///
    /// Returns the failure text the host surfaces as `STATUS_FAILED`.
    pub fn serve(&mut self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        match method {
            METHOD_DESCRIBE => {
                let _ = payload;
                Ok(serde_json::to_vec(&json!({
                    "name": PLUGIN_NAME,
                    "service": SERVICE_NAME,
                    "abiVersion": ABI_VERSION,
                }))
                .unwrap_or_default())
            }
            METHOD_MEASURE => self.serve_measure(payload),
            METHOD_ESTIMATE_MESSAGE => serve_estimate_message(payload),
            METHOD_FOLD_TOKEN_USAGE => serve_fold(payload, |events| {
                Ok(
                    serde_json::to_value(crate::usage_projection::fold_token_usage(events))
                        .unwrap_or(Value::Null),
                )
            }),
            METHOD_FOLD_CONTEXT_PRESSURE => serve_fold(payload, |events| {
                crate::usage_projection::fold_context_pressure(events)
                    .and_then(|state| serde_json::to_value(state).map_err(|e| e.to_string()))
            }),
            METHOD_FOLD_CONTEXT_BREAKDOWN => serve_fold(payload, |events| {
                crate::breakdown_projection::fold_context_breakdown(events)
                    .map(|state| serde_json::to_value(state).unwrap_or(Value::Null))
            }),
            other => Err(format!("unknown token-meter method {other}")),
        }
    }
}

impl ShellState {
    /// The `measure` face: `{id, requestHeader?}` → `TokenMeasurement`.
    ///
    /// The fold catches up through the `session-persistence` read face
    /// incrementally (from the consumed offset; the dsh `_sync`
    /// while-loop shape), then the measurement resolves the route seams
    /// over the `llm` face. A fold refusal leaves the consumed offset
    /// untouched, so the same malformed event fails identically on
    /// every retry.
    fn serve_measure(&mut self, payload: &[u8]) -> Result<Vec<u8>, String> {
        let request: Value = serde_json::from_slice(payload)
            .map_err(|error| format!("measure payload must be JSON: {error}"))?;
        let id = request
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "id must be a string".to_owned())?
            .to_owned();
        let request_header = match request.get("requestHeader") {
            Some(Value::Null) | None => None,
            Some(header) => Some(
                serde_json::from_value::<fr_wire::event::EpochHeader>(header.clone())
                    .map_err(|error| format!("requestHeader decode: {error}"))?,
            ),
        };
        let Self { link, sessions } = self;
        let state = sessions.entry(&id);
        let from = state.consumed_events;
        let read = json!({ "id": id, "from": from });
        let bytes = serde_json::to_vec(&read)
            .map_err(|error| format!("session read encode failed: {error}"))?;
        let reply = link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            &bytes,
        )?;
        let decoded: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("session read reply decode failed: {error}"))?;
        let events = decoded
            .get("events")
            .and_then(Value::as_array)
            .ok_or_else(|| "session read reply requires an events array".to_owned())?;
        for raw in events {
            let event: SessionEvent = serde_json::from_value(raw.clone())
                .map_err(|error| format!("session event decode failed: {error}"))?;
            fold_event(state, &event)?;
            state.consumed_events += 1;
        }
        let image_prices = Box::new(
            |provider: &str, model: &str, images: &[fr_wire::content::ContentBlock]| {
                route_image_prices(link, provider, model, images)
            },
        );
        let file_text = Some(Box::new(|reference: &fr_wire::content::FileAttachmentRef| {
            route_file_text(link, reference)
        })
            as Box<
                dyn Fn(&fr_wire::content::FileAttachmentRef) -> Result<String, String>,
            >);
        let resolvers = RouteResolvers {
            image_prices,
            file_text,
        };
        let measurement = measure(state, request_header.as_ref(), &resolvers)?;
        serde_json::to_vec(&measurement.to_wire())
            .map_err(|error| format!("measurement encode failed: {error}"))
    }
}

/// Resolves the route's per-occurrence image prices over the `llm`
/// face (the dsh `_routeImagePricing` + `priceImages` pair): `None`
/// keeps the fixed heuristic, mirroring the dsh soft-get where a route
/// with no declared pricing answers `undefined`.
fn route_image_prices(
    link: &HostLink,
    provider: &str,
    model: &str,
    images: &[fr_wire::content::ContentBlock],
) -> Option<Vec<fr_llm::adapter::ImageRequestPrice>> {
    let request = json!({
        "provider": provider,
        "model": model,
        "images": images,
    });
    let bytes = serde_json::to_vec(&request).ok()?;
    let reply = link
        .require(
            fr_wire::services::llm::SERVICE_NAME,
            fr_wire::services::llm::METHOD_IMAGE_REQUEST_PRICING,
            &bytes,
        )
        .ok()?;
    let decoded: Value = serde_json::from_slice(&reply).ok()?;
    let prices = decoded.get("prices")?;
    if prices.is_null() {
        return None;
    }
    let prices = prices.as_array()?;
    prices
        .iter()
        .map(|price| {
            // The price shape decodes field-wise (`ImageRequestPrice`
            // is a plain struct without Deserialize).
            Some(fr_llm::adapter::ImageRequestPrice {
                visual_tokens: price.get("visualTokens")?.as_u64()?,
                text: price.get("text")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

/// Resolves the exact request-time file handle text over the `llm`
/// face (the dsh `fileRequestText`).
fn route_file_text(
    link: &HostLink,
    reference: &fr_wire::content::FileAttachmentRef,
) -> Result<String, String> {
    let request = json!({ "ref": reference });
    let bytes = serde_json::to_vec(&request)
        .map_err(|error| format!("file text encode failed: {error}"))?;
    let reply = link.require(
        fr_wire::services::llm::SERVICE_NAME,
        fr_wire::services::llm::METHOD_FILE_REQUEST_TEXT,
        &bytes,
    )?;
    let decoded: Value = serde_json::from_slice(&reply)
        .map_err(|error| format!("file text reply decode failed: {error}"))?;
    decoded
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "file text reply requires a text".to_owned())
}

/// The `estimate-message` face (the dsh instance `estimateMessage`):
/// `{message}` → `{tokens}`.
fn serve_estimate_message(payload: &[u8]) -> Result<Vec<u8>, String> {
    let request: Value = serde_json::from_slice(payload)
        .map_err(|error| format!("estimate payload must be JSON: {error}"))?;
    let message: fr_wire::message::Message = serde_json::from_value(
        request
            .get("message")
            .cloned()
            .ok_or_else(|| "estimate requires a message".to_owned())?,
    )
    .map_err(|error| format!("message decode: {error}"))?;
    let tokens = crate::estimate::estimate_message(&message);
    Ok(serde_json::to_vec(&json!({ "tokens": tokens })).unwrap_or_default())
}

/// A projection-unit fold: the relay hands the full log, the endpoint
/// answers the unit state (Q110′A′: no outbound requires inside).
fn serve_fold(
    payload: &[u8],
    fold: impl Fn(&[SessionEvent]) -> Result<Value, String>,
) -> Result<Vec<u8>, String> {
    let request: Value = serde_json::from_slice(payload)
        .map_err(|error| format!("fold payload must be JSON: {error}"))?;
    let events = request
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| "fold payload requires an events array".to_owned())?;
    let events: Vec<SessionEvent> = events
        .iter()
        .map(|raw| {
            serde_json::from_value(raw.clone())
                .map_err(|error| format!("session event decode failed: {error}"))
        })
        .collect::<Result<_, _>>()?;
    let state = fold(&events)?;
    Ok(serde_json::to_vec(&json!({ "state": state })).unwrap_or_default())
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
    let link = if host.is_null() {
        None
    } else {
        // SAFETY: the vtable contract guarantees validity for this call;
        // copying by value is the sanctioned pattern (dylib-plugin.md).
        Some(HostLink::new(unsafe { *host }))
    };
    // Fail-closed: a refused registration declines the activation.
    ShellState::try_new(link).map_or(std::ptr::null_mut(), |state| {
        Box::into_raw(Box::new(state)).cast::<std::ffi::c_void>()
    })
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
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
    match state.serve(method, payload) {
        Ok(reply) => {
            // SAFETY: host buffer contract.
            if unsafe { write_out(&reply, out, cap, out_len) } {
                STATUS_OK
            } else {
                STATUS_CAPACITY
            }
        }
        Err(message) => {
            let body = serde_json::json!({ "error": message }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            STATUS_FAILED
        }
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
    // Reclaim the state; the kernel reverses the registrations LIFO on
    // withdrawal before this runs.
    // SAFETY: the handle was produced by `setup` and torn down exactly once.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}

/// Export table for the fr-token-meter dylib artifact; the host resolves
/// this symbol by name at load time.
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
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// One recorded outbound require.
    #[derive(Debug, Clone)]
    struct Outbound {
        service: String,
        method: u32,
        payload: Value,
    }

    /// A fake host recording every grant and require, with scripted
    /// replies per (service, method).
    struct FakeHost {
        grants: Mutex<Vec<(String, String)>>,
        requires: Mutex<Vec<Outbound>>,
        replies: Mutex<HashMap<u32, Vec<u8>>>,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                grants: Mutex::new(Vec::new()),
                requires: Mutex::new(Vec::new()),
                replies: Mutex::new(HashMap::new()),
            }
        }

        fn script(&self, method: u32, reply: Vec<u8>) {
            self.replies.lock().expect("lock").insert(method, reply);
        }

        fn handle(&self) -> *mut std::os::raw::c_void {
            std::ptr::from_ref(self)
                .cast::<std::ffi::c_void>()
                .cast_mut()
        }
    }

    unsafe extern "C" fn fake_grant(
        host: *mut std::os::raw::c_void,
        target: *const u8,
        target_len: usize,
        granted: *const u8,
        granted_len: usize,
    ) -> i32 {
        // SAFETY: the fake vtable's own contract — the handle is the
        // FakeHost reference the test set.
        let host = unsafe { &*host.cast::<FakeHost>() };
        // SAFETY: the name buffers are valid for their lengths per the
        // vtable contract.
        // SAFETY: the name buffers are valid for their lengths per the
        // vtable contract.
        let target = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(target, target_len)).into_owned()
        };
        // SAFETY: same buffer contract as `target`.
        let granted = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(granted, granted_len)).into_owned()
        };
        host.grants.lock().expect("lock").push((target, granted));
        STATUS_OK
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
        // FakeHost reference the test set.
        let host = unsafe { &*host.cast::<FakeHost>() };
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
        let scripted = host.replies.lock().expect("lock").get(&method).cloned();
        let reply = scripted.unwrap_or_else(|| br"{}".to_vec());
        // SAFETY: caller-owned out-buffer contract.
        if unsafe { write_out(&reply, out, cap, out_len) } {
            STATUS_OK
        } else {
            STATUS_CAPACITY
        }
    }

    unsafe extern "C" fn noop_emit(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_FAILED
    }

    unsafe extern "C" fn noop_dispatch(
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

    fn linked_state(scripted: &[(u32, Vec<u8>)]) -> (ShellState, std::sync::Arc<FakeHost>) {
        let host = std::sync::Arc::new(FakeHost::new());
        for (method, reply) in scripted {
            host.script(*method, reply.clone());
        }
        let exports = HostExports {
            host_handle: host.handle(),
            require: fake_require,
            grant: fake_grant,
            emit: noop_emit,
            dispatch: noop_dispatch,
        };
        let state = ShellState::try_new(Some(HostLink::new(exports)))
            .expect("the wiring sequence completes");
        (state, host)
    }

    #[test]
    fn descriptor_declares_service_injects_and_quiet_listen() {
        let parsed: Value = serde_json::from_slice(DESCRIPTOR).expect("descriptor is JSON");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"], json!(["token-meter"]));
        assert_eq!(
            parsed["inject"],
            json!(["session-persistence", "session-projections", "llm"])
        );
        assert_eq!(parsed["listen"], json!({}));
    }

    #[test]
    fn method_constants_match_the_fr_wire_table() {
        assert_eq!(SERVICE_NAME, fr_wire::services::token_meter::SERVICE_NAME);
        assert_eq!(
            METHOD_DESCRIBE,
            fr_wire::services::token_meter::METHOD_DESCRIBE
        );
        assert_eq!(
            METHOD_MEASURE,
            fr_wire::services::token_meter::METHOD_MEASURE
        );
        assert_eq!(
            METHOD_ESTIMATE_MESSAGE,
            fr_wire::services::token_meter::METHOD_ESTIMATE_MESSAGE
        );
        assert_eq!(
            METHOD_FOLD_TOKEN_USAGE,
            fr_wire::services::token_meter::METHOD_FOLD_TOKEN_USAGE
        );
        assert_eq!(
            METHOD_FOLD_CONTEXT_PRESSURE,
            fr_wire::services::token_meter::METHOD_FOLD_CONTEXT_PRESSURE
        );
        assert_eq!(
            METHOD_FOLD_CONTEXT_BREAKDOWN,
            fr_wire::services::token_meter::METHOD_FOLD_CONTEXT_BREAKDOWN
        );
    }

    /// The setup wiring order: the relay's fold-call grant first, then
    /// the three unit registrations with the dsh keys and state
    /// versions.
    #[test]
    fn setup_grants_then_registers_three_units() {
        let (state, host) = linked_state(&[]);
        let _ = state;
        let grants = host.grants.lock().expect("lock");
        assert_eq!(
            *grants,
            vec![(
                fr_wire::services::session_projections::SERVICE_NAME.to_owned(),
                SERVICE_NAME.to_owned()
            )]
        );
        let requires = host.requires.lock().expect("lock");
        assert_eq!(requires.len(), 3, "exactly three registrations");
        let keys = [
            ("tokenUsage", 2, METHOD_FOLD_TOKEN_USAGE),
            ("contextPressure", 5, METHOD_FOLD_CONTEXT_PRESSURE),
            ("contextBreakdown", 6, METHOD_FOLD_CONTEXT_BREAKDOWN),
        ];
        for (outbound, (key, version, fold_method)) in requires.iter().zip(keys) {
            assert_eq!(outbound.service, "session-projections");
            assert_eq!(
                outbound.method,
                fr_wire::services::session_projections::METHOD_REGISTER
            );
            assert_eq!(outbound.payload["key"], json!(key));
            assert_eq!(outbound.payload["stateVersion"], json!(version));
            assert_eq!(
                outbound.payload["fold"],
                json!({"service": SERVICE_NAME, "method": fold_method})
            );
        }
    }

    /// The measure face catches up through the read face incrementally
    /// (from the consumed offset) and answers the measurement.
    #[test]
    fn measure_catches_up_and_answers() {
        let step_event = |seq: u64, kind: &'static str| {
            let data = if kind == "start" {
                fr_wire::event::SessionEventData::StepStart(fr_wire::event::StepBoundsData {
                    turn: 1,
                    step: 0,
                })
            } else {
                fr_wire::event::SessionEventData::StepEnd(fr_wire::event::StepBoundsData {
                    turn: 1,
                    step: 0,
                })
            };
            fr_wire::event::SessionEvent {
                seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
                time: 1,
                data,
                ignorable: None,
                surface_op: None,
                source_event_seqs: None,
            }
        };
        let assistant = fr_wire::event::SessionEvent {
            seq: fr_wire::SessionSeq::new(2).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: fr_wire::event::SessionEventData::AssistantMessage(
                fr_wire::event::AssistantMessageData {
                    turn: 1,
                    step: 0,
                    message: fr_wire::message::Message::Known(Box::new(
                        fr_wire::message::KnownMessage::Assistant {
                            id: fr_wire::ids::MessageId::new("a"),
                            content: vec![fr_wire::content::ContentBlock::Known(
                                fr_wire::content::KnownContentBlock::Text {
                                    text: "abcdefgh".to_owned(),
                                },
                            )],
                            source: fr_wire::message::MessageSource::Known(
                                fr_wire::message::KnownMessageSource::Model {
                                    provider: "p".to_owned(),
                                    model: "m".to_owned(),
                                    replay_state: None,
                                    extra: fr_wire::content::RawObject::new(),
                                },
                            ),
                            extra: fr_wire::content::RawObject::new(),
                        },
                    )),
                    stream: Vec::new(),
                    usage: None,
                    interrupted: None,
                },
            ),
            ignorable: None,
            surface_op: Some(fr_wire::event::SurfaceOp::Append(fr_wire::event::AppendOp)),
            source_event_seqs: None,
        };
        let events = vec![
            serde_json::to_value(step_event(1, "start")).expect("step start"),
            serde_json::to_value(&assistant).expect("assistant"),
            serde_json::to_value(step_event(3, "end")).expect("step end"),
        ];
        let read_reply = serde_json::to_vec(&json!({
            "events": events,
            "total": 3,
            "header": null,
            "inheritedEventCount": 0,
        }))
        .expect("reply");
        let (mut state, host) = linked_state(&[(
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            read_reply,
        )]);

        let payload = json!({ "id": "s1" }).to_string();
        let reply = state
            .serve(METHOD_MEASURE, payload.as_bytes())
            .expect("measure");
        let reply: Value = serde_json::from_slice(&reply).expect("json");
        let expected = 8_u64.div_ceil(4) + 4 + crate::estimate::ROLE_OVERHEAD;
        assert_eq!(reply["logRevision"], json!(3));
        assert_eq!(reply["totalTokens"], json!(expected));
        assert_eq!(reply["surfaceTokens"], json!(expected));
        assert_eq!(reply["baseline"]["kind"], json!("estimated"));
        assert_eq!(reply["nodes"][0]["seq"], json!(2));
        assert_eq!(reply["nodes"][0]["tokens"], json!(expected));

        // The read rode the from offset (0 on the first catch-up).
        let read = {
            let requires = host.requires.lock().expect("lock");
            requires
                .iter()
                .find(|outbound| {
                    outbound.service == "session-persistence"
                        && outbound.method
                            == fr_wire::services::session_persistence::METHOD_SESSION_READ
                })
                .cloned()
                .expect("the read")
        };
        assert_eq!(read.payload["id"], json!("s1"));
        assert_eq!(read.payload["from"], json!(0));
    }

    /// The fold endpoints answer `{state}` over the full log; the
    /// estimate face prices one message.
    #[test]
    fn fold_endpoints_and_estimate_face_answer() {
        let (mut state, _host) = linked_state(&[]);
        let payload = json!({ "events": [] }).to_string();
        for method in [
            METHOD_FOLD_TOKEN_USAGE,
            METHOD_FOLD_CONTEXT_PRESSURE,
            METHOD_FOLD_CONTEXT_BREAKDOWN,
        ] {
            let reply = state.serve(method, payload.as_bytes()).expect("fold");
            let reply: Value = serde_json::from_slice(&reply).expect("json");
            assert!(reply.get("state").is_some(), "unit state key for {method}");
        }
        let zero = state
            .serve(METHOD_FOLD_TOKEN_USAGE, payload.as_bytes())
            .expect("token usage fold");
        let zero: Value = serde_json::from_slice(&zero).expect("json");
        assert_eq!(
            zero["state"],
            json!({
                "totals": {
                    "uncachedInputTokens": 0,
                    "outputTokens": 0,
                    "cacheReadTokens": 0,
                    "cacheWriteTokens": 0,
                },
                "last": null,
            })
        );

        let message = json!({
            "message": {
                "id": "m",
                "role": "user",
                "content": [{"type": "text", "text": "abcdefgh"}],
                "source": {"kind": "user"},
            },
        })
        .to_string();
        let reply = state
            .serve(METHOD_ESTIMATE_MESSAGE, message.as_bytes())
            .expect("estimate");
        let reply: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(
            reply["tokens"],
            json!(8_u64.div_ceil(4) + 4 + crate::estimate::ROLE_OVERHEAD)
        );
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        let (mut state, _host) = linked_state(&[]);
        let failure = state.serve(9, b"{}").expect_err("unknown method");
        assert_eq!(failure, "unknown token-meter method 9");
    }
}
