//! The dylib shell: the descriptor, the baked storage root, the emit
//! wiring, and the export table (the fr-shell export shape).

use std::os::raw::c_void;
use std::path::PathBuf;
use std::sync::Arc;

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};
use serde_json::json;

use crate::domain::DomainChanged;
use crate::facility::DomainFacility;
use crate::service::{PLUGIN_NAME, StorageDomainService};

/// The static descriptor: one provided service, no injected seams, no
/// listeners (the storage root rides the baked environment chain).
const DESCRIPTOR: &str =
    "{\"name\":\"fr-storage\",\"provide\":[\"storage-domain\"],\"inject\":[],\"listen\":{}}";

/// One copied host vtable (the fr-shell `HostLink` shape).
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
// the fr-shell judgment carries the same declaration.
unsafe impl Send for HostLink {}
// SAFETY: see `Send` — every host call goes through the same serialized
// entry point.
unsafe impl Sync for HostLink {}

impl HostLink {
    /// Copies one vtable.
    #[must_use]
    pub(crate) const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Publishes one kernel event with a JSON payload (the dsh
    /// `domain/changed` carrier; observer failures are contained by the
    /// host's status reply and surfaced as a log on the host side).
    pub(crate) fn emit(&self, event: &str, payload: &str) {
        // SAFETY: the vtable contract guarantees validity for this call;
        // byte-string arguments are read-only for the call's duration.
        let _status = unsafe {
            (self.host.emit)(
                self.host.host_handle,
                event.as_ptr(),
                event.len(),
                payload.as_ptr(),
                payload.len(),
            )
        };
    }
}

/// The plugin state behind the export-table handle.
#[derive(Debug)]
pub struct StorageState {
    facility: Arc<DomainFacility>,
    service: StorageDomainService,
}

/// The baked storage root (`FR_STORAGE_ROOT` wins, else the Fly Rafter
/// home plus `storage`; an unresolvable home degrades to the relative
/// default, whose reads find no logs — the fr-session chain).
#[must_use]
pub fn baked_storage_root_from(
    storage_root: Option<&str>,
    home: Option<&str>,
    os_home: Option<&str>,
) -> PathBuf {
    if let Some(root) = storage_root {
        return PathBuf::from(root);
    }
    let base = match (home, os_home) {
        (Some(home), _) => PathBuf::from(home),
        (None, Some(os_home)) => PathBuf::from(os_home).join(".fly-rafter"),
        (None, None) => PathBuf::from(".fly-rafter"),
    };
    base.join("storage")
}

/// The deployment's baked storage root from the process environment.
#[must_use]
pub fn baked_storage_root() -> PathBuf {
    baked_storage_root_from(
        std::env::var("FR_STORAGE_ROOT").ok().as_deref(),
        std::env::var("FR_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// Builds the plugin state: resolves the baked root, mounts the
/// facility, and wires `domain/changed` to the host bus.
///
/// Returns `None` when the storage root cannot be created; a poisoned
/// mount is unreachable at first construction, so nothing else fails.
#[must_use]
pub fn build_state(link: Option<HostLink>) -> Option<StorageState> {
    let facility = Arc::new(DomainFacility::new());
    let root = baked_storage_root();
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!("fr-storage: storage root create failed: {error}");
        return None;
    }
    let emit_link = link;
    let emit: crate::domain::EmitFace = Arc::new(move |changed: &DomainChanged| {
        let Some(link) = emit_link else {
            return;
        };
        let operation = match changed.operation {
            crate::domain::DomainOperation::Put => "put",
            crate::domain::DomainOperation::Deleted => "deleted",
        };
        let payload = json!({
            "domain": changed.domain,
            "table": changed.table,
            "key": changed.key,
            "operation": operation,
            "value": changed.value,
        });
        link.emit("domain/changed", &payload.to_string());
    });
    if let Err(error) = facility.mount(root, emit) {
        eprintln!("fr-storage: facility mount failed: {error}");
        return None;
    }
    let service = StorageDomainService::new(Arc::clone(&facility));
    Some(StorageState { facility, service })
}

/// Serves one method invocation against the storage face.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
pub fn dispatch(state: &StorageState, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
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
    let Some(state) = build_state(link) else {
        // Decline activation (null handle): a storage seam without a
        // usable root must not exist.
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
    let state = unsafe { &*(handle.cast::<StorageState>()) };
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
    // once; the facility close drains every domain's write chain and
    // joins the worker threads before the vtable goes undefined.
    let state = unsafe { Box::from_raw(handle.cast::<StorageState>()) };
    let _ = state.facility.close_all();
    drop(state);
}

/// Export table for the fr-storage dylib artifact.
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
