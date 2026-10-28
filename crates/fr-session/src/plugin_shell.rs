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
//! The artifact provides two services (Q105′A): `session-persistence`
//! (selectors 0-9; the store face's shell wiring is the Phase 18
//! assembler's Q44-B fabric-INIT work) and `session-projections`
//! (selectors 1000+, wired here now). The projections relay's outbound
//! fold call needs the copied host vtable, so a null host declines the
//! whole activation fail-closed (the approval-shell judgment: a
//! half-wired seam must not exist). The relay's read source is the baked
//! sessions root — dougong delivers no config to dylib artifacts, so the
//! root carries the baked-config judgment: `FR_SESSIONS_ROOT` wins, else
//! the Fly Rafter home (`FR_HOME`, else the OS home's `.fly-rafter`)
//! plus `sessions`, the dsh `dshHomePath('sessions')` equivalent. An
//! absent root is a fresh deployment (no stored logs), never a failure.
//!
//! The host serializes every foreign call per plugin instance, so the
//! vocabulary and registry need no locking (the same contract the ABI
//! export-table documentation gives `setup`/`call` interleaving).

use std::path::PathBuf;
use std::sync::Arc;

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};

use crate::persist::backend::{DEFAULT_COMPRESSION, cold_read_stored_log};
use crate::projections::{ColdLog, ColdLogRead, FoldCaller, FoldEndpoint, Projections};
use crate::vocabulary::EventVocabulary;
use fr_wire::ids::SessionId;

/// The provided-service name consumers inject (the dsh domain service
/// name; batch 2a renamed it from the crate name — every consumer already
/// required this spelling, and PRD 4.4 lists it). The service-face
/// constants live in the shell-free service module; the re-exports keep
/// this path stable.
pub use crate::service::{METHOD_DECLARE_EVENT_TYPES, METHOD_DESCRIBE, SERVICE_NAME};

/// The projections service's name and selectors, re-exported for path
/// stability (the constants live in the shell-free projections module).
pub use crate::projections::METHOD_REGISTER as METHOD_PROJECTIONS_REGISTER;
pub use crate::projections::METHOD_STATE_OF as METHOD_PROJECTIONS_STATE_OF;
pub use crate::projections::SERVICE_NAME as PROJECTIONS_SERVICE_NAME;

/// The descriptor the loader consumes: both provided services over the
/// dynamic channel, no listeners in M1 (the store wiring is a later
/// slice).
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-session\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"session-persistence\",\"session-projections\"],",
    "\"inject\":[],\"listen\":{}}"
)
.as_bytes();

/// Upper bound for one outbound round trip on the growable buffer
/// protocol (the dougong host documents a 16 MiB ceiling).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// The dylib artifact's plugin name.
const PLUGIN_NAME: &str = "fr-session";

/// One copied host vtable with the outbound arm this shell uses:
/// `require` (the projections relay's fold call).
#[derive(Debug, Clone, Copy)]
pub struct HostLink {
    host: HostExports,
}

// SAFETY: the ABI vtable contract allows require from any plugin thread,
// concurrently; the copy stays valid until this activation's `teardown`
// returns, and this shell never outlives its activation.
unsafe impl Send for HostLink {}

// SAFETY: sharing one copy only issues concurrent vtable calls, which the
// same contract explicitly allows.
unsafe impl Sync for HostLink {}

impl HostLink {
    /// Wraps a vtable copied at `setup`.
    const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Performs one synchronous cross-plugin call (growable buffer
    /// protocol).
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
}

/// The projections relay's read source: the baked root's stored logs.
struct WireColdRead {
    root: PathBuf,
}

impl ColdLogRead for WireColdRead {
    fn cold_read(&self, id: &str, known: &EventVocabulary) -> Result<Option<ColdLog>, String> {
        let outcome = cold_read_stored_log(
            &self.root,
            DEFAULT_COMPRESSION,
            known.clone(),
            &SessionId::new(id.to_owned()),
        )
        .map_err(|error| error.to_string())?;
        outcome
            .map(|stored| {
                Ok(ColdLog {
                    header: serde_json::to_value(&stored.meta)
                        .map_err(|error| format!("projection header encode failed: {error}"))?,
                    inherited_event_count: stored.inherited_event_count,
                    events: stored
                        .events
                        .iter()
                        .map(serde_json::to_value)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| format!("projection events encode failed: {error}"))?,
                })
            })
            .transpose()
    }
}

/// The fold-call seam over one copied vtable.
struct WireFoldCaller {
    link: HostLink,
}

impl FoldCaller for WireFoldCaller {
    fn call_fold(
        &self,
        endpoint: &FoldEndpoint,
        log: &ColdLog,
    ) -> Result<serde_json::Value, String> {
        let payload = serde_json::json!({
            "header": log.header,
            "inheritedEventCount": log.inherited_event_count,
            "events": log.events,
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("projection fold encode failed: {error}"))?;
        let reply = self
            .link
            .require(&endpoint.service, endpoint.method, &bytes)?;
        let reply: serde_json::Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("projection fold reply decode failed: {error}"))?;
        reply
            .get("state")
            .cloned()
            .ok_or_else(|| "projection fold reply requires a state".to_owned())
    }
}

/// The shell state: the vocabulary aggregate and the projections service.
pub struct ShellState {
    vocabulary: EventVocabulary,
    projections: Projections,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("vocabulary", &self.vocabulary)
            .field("projections", &self.projections)
            .finish_non_exhaustive()
    }
}

impl ShellState {
    /// Assembles the state over the vocabulary and the projections
    /// service; `None` declines the activation fail-closed (the relay's
    /// fold call needs the copied vtable — the approval-shell judgment:
    /// a half-wired seam must not exist).
    #[must_use]
    pub fn try_new(link: Option<HostLink>) -> Option<Self> {
        let link = link?;
        Some(Self {
            vocabulary: EventVocabulary::core(),
            projections: Projections::new(
                Arc::new(WireColdRead {
                    root: baked_sessions_root(),
                }),
                Arc::new(WireFoldCaller { link }),
            ),
        })
    }

    /// Assembles the state over explicit pieces (the vocabulary-face
    /// tests drive the shell without a host).
    #[must_use]
    pub const fn with_parts(vocabulary: EventVocabulary, projections: Projections) -> Self {
        Self {
            vocabulary,
            projections,
        }
    }
}

/// The deployment's baked sessions root (the baked-config judgment:
/// `FR_SESSIONS_ROOT` wins, else the Fly Rafter home plus `sessions`).
fn baked_sessions_root() -> PathBuf {
    let sessions_root = std::env::var("FR_SESSIONS_ROOT").ok();
    let home = std::env::var("FR_HOME").ok();
    let os_home = std::env::var("HOME").ok();
    baked_sessions_root_from(
        sessions_root.as_deref(),
        home.as_deref(),
        os_home.as_deref(),
    )
}

/// The pure form of the baked root (dsh `dshHomePath('sessions')`: the
/// configured home plus the `sessions` segment; an unresolvable home
/// degrades to the relative default, whose reads find no logs).
#[must_use]
pub fn baked_sessions_root_from(
    sessions_root: Option<&str>,
    home: Option<&str>,
    os_home: Option<&str>,
) -> PathBuf {
    if let Some(root) = sessions_root {
        return PathBuf::from(root);
    }
    let base = match (home, os_home) {
        (Some(home), _) => PathBuf::from(home),
        (None, Some(os_home)) => PathBuf::from(os_home).join(".fly-rafter"),
        (None, None) => PathBuf::from(".fly-rafter"),
    };
    base.join("sessions")
}

/// Serves one method invocation against the shell state: a pure
/// (JSON-in, JSON-out) function so the shell surface stays testable.
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`.
pub fn dispatch(method: u32, payload: &[u8], state: &mut ShellState) -> Result<Vec<u8>, String> {
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
            state
                .vocabulary
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
            "knownTypes": state.vocabulary.known_count(),
        }))
        .unwrap_or_default()),
        METHOD_PROJECTIONS_REGISTER | METHOD_PROJECTIONS_STATE_OF => {
            let ShellState {
                vocabulary,
                projections,
            } = state;
            projections.serve(method, payload, vocabulary)
        }
        other if other >= METHOD_PROJECTIONS_REGISTER => {
            Err(format!("unknown session-projections method {other}"))
        }
        other => Err(format!("unknown session-persistence method {other}")),
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

// ABI v3 hands the outbound `HostExports` vtable here; the projections
// relay copies it by value (the sanctioned pattern) and the copy lives no
// longer than this activation — the state it feeds is dropped in
// `teardown`, before the documented UB window opens.
unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    let link = if host.is_null() {
        None
    } else {
        // SAFETY: the vtable contract guarantees validity for this call;
        // copying by value is the sanctioned pattern (dylib-plugin.md).
        Some(HostLink::new(unsafe { *host }))
    };
    let Some(state) = ShellState::try_new(link) else {
        // Decline activation (null handle): the relay cannot exist
        // without its fold-call leg.
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(state)).cast::<std::ffi::c_void>()
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
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
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
    let reply = match dispatch(method, payload, state) {
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
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
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
    use super::{
        DESCRIPTOR, METHOD_DECLARE_EVENT_TYPES, METHOD_DESCRIBE, METHOD_PROJECTIONS_REGISTER,
        METHOD_PROJECTIONS_STATE_OF, ShellState, baked_sessions_root_from, dispatch,
    };
    use crate::persist::{JsonlBackend, SESSION_FORMAT_VERSION};
    use crate::projections::{ColdLog, ColdLogRead, FoldCaller, Projections};
    use crate::vocabulary::EventVocabulary;
    use dougong_abi::HostExports;
    use fr_wire::event::{SessionEvent, SessionEventData, UnknownEventData};
    use fr_wire::ids::{SessionId, SessionSeq};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// A read source with no logs (the vocabulary-face tests).
    struct EmptyRead;

    impl ColdLogRead for EmptyRead {
        fn cold_read(
            &self,
            _id: &str,
            _known: &EventVocabulary,
        ) -> Result<Option<ColdLog>, String> {
            Ok(None)
        }
    }

    /// A fold caller that always fails (the vocabulary-face tests never
    /// reach it).
    struct NoFold;

    impl FoldCaller for NoFold {
        fn call_fold(
            &self,
            _endpoint: &super::FoldEndpoint,
            _log: &ColdLog,
        ) -> Result<Value, String> {
            Err("no fold in this test".to_owned())
        }
    }

    fn shell_state() -> ShellState {
        ShellState::with_parts(
            EventVocabulary::core(),
            Projections::new(Arc::new(EmptyRead), Arc::new(NoFold)),
        )
    }

    #[test]
    fn declare_accepts_new_types_and_refuses_duplicates() {
        let mut state = shell_state();
        let payload = serde_json::json!({ "types": ["compaction/start", "compaction/end"] });
        let reply = dispatch(
            METHOD_DECLARE_EVENT_TYPES,
            payload.to_string().as_bytes(),
            &mut state,
        )
        .expect("first declare");
        let reply: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply, serde_json::json!({ "declared": 2 }));

        let duplicate = serde_json::json!({ "types": ["compaction/start"] });
        let error = dispatch(
            METHOD_DECLARE_EVENT_TYPES,
            duplicate.to_string().as_bytes(),
            &mut state,
        )
        .expect_err("duplicate");
        assert_eq!(
            error,
            "session event type \"compaction/start\" is already declared"
        );
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let mut state = shell_state();
        let reply = dispatch(METHOD_DESCRIBE, b"{}", &mut state).expect("describe");
        let reply: Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], "session-persistence");
        assert_eq!(reply["formatVersion"], 4);
        assert_eq!(reply["knownTypes"], 14);
    }

    #[test]
    fn descriptor_declares_both_provided_services() {
        let parsed: Value = serde_json::from_slice(DESCRIPTOR).expect("descriptor is JSON");
        assert_eq!(
            parsed["provide"],
            serde_json::json!(["session-persistence", "session-projections"])
        );
        assert_eq!(parsed["inject"], serde_json::json!([]));
    }

    #[test]
    fn unknown_methods_fail_per_band() {
        let mut state = shell_state();
        assert_eq!(
            dispatch(99, b"{}", &mut state).expect_err("persistence band"),
            "unknown session-persistence method 99"
        );
        assert_eq!(
            dispatch(1002, b"{}", &mut state).expect_err("projections band"),
            "unknown session-projections method 1002"
        );
    }

    #[test]
    fn baked_root_follows_the_home_expression() {
        assert_eq!(
            baked_sessions_root_from(Some("/srv/sessions"), None, None),
            PathBuf::from("/srv/sessions")
        );
        assert_eq!(
            baked_sessions_root_from(None, Some("/x/fr-home"), None),
            PathBuf::from("/x/fr-home/sessions")
        );
        assert_eq!(
            baked_sessions_root_from(None, None, Some("/u/home")),
            PathBuf::from("/u/home/.fly-rafter/sessions")
        );
        assert_eq!(
            baked_sessions_root_from(None, None, None),
            PathBuf::from(".fly-rafter/sessions")
        );
    }

    /// A fake host whose require captures the fold payload and replies a
    /// fixed state (the real vtable is exercised — only the far side is
    /// scripted).
    #[derive(Default)]
    struct FoldHost {
        requires: Mutex<Vec<(String, u32, Value)>>,
        state: Value,
    }

    // SAFETY: the recorded state is only touched behind its mutex from
    // the vtable callbacks, which the ABI contract may invoke from any
    // plugin thread.
    unsafe impl Send for FoldHost {}
    // SAFETY: same mutex-only access contract as `Send`.
    unsafe impl Sync for FoldHost {}

    unsafe extern "C" fn fold_require(
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
        // FoldHost reference the test set.
        let host = unsafe { &*(host.cast::<FoldHost>()) };
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
        host.requires
            .lock()
            .expect("lock")
            .push((service, method, payload));
        let reply = json!({ "state": host.state }).to_string();
        // SAFETY: caller-owned out-buffer contract.
        if unsafe { dougong_abi::write_out(reply.as_bytes(), out, cap, out_len) } {
            dougong_abi::STATUS_OK
        } else {
            dougong_abi::STATUS_CAPACITY
        }
    }

    unsafe extern "C" fn ok_emit(
        _host: *mut std::os::raw::c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_OK
    }

    unsafe extern "C" fn ok_grant(
        _host: *mut std::os::raw::c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_OK
    }

    /// One temp directory removed on drop (the backend-test pattern).
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-session-shell-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_nanos())
            ));
            let _ignored = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_dir_all(&self.0);
        }
    }

    fn plugin_event(seq: u64, type_name: &str, data: &Value) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq in range"),
            time: 0,
            data: SessionEventData::Unknown(UnknownEventData {
                type_name: type_name.to_owned(),
                data: data.as_object().cloned().unwrap_or_default(),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    #[test]
    fn state_of_relays_over_a_real_stored_log() {
        let root = TempRoot::new("relay");
        // Write one stored log carrying a plugin-declared event.
        let mut writer_vocabulary = EventVocabulary::core();
        writer_vocabulary
            .declare(&["permission/preset".to_owned()])
            .expect("declare");
        let backend = JsonlBackend::builder(root.path(), writer_vocabulary)
            .batch_max_delay(std::time::Duration::from_millis(30))
            .build()
            .expect("backend builds");
        let header = fr_wire::event::SessionHeader {
            version: SESSION_FORMAT_VERSION,
            id: SessionId::new("s-relay".to_owned()),
            created_at: 0,
            cwd: None,
            parent_session: None,
            agent_preset: None,
            delegation_depth: None,
            origin: None,
            is_seeded: false,
        };
        let handle = backend.create(&header, 0).expect("create");
        let batch = vec![plugin_event(
            0,
            "permission/preset",
            &json!({ "preset": "read-only" }),
        )];
        handle.append(&batch).expect("append");
        handle.flush().expect("flush");
        drop(handle);
        drop(backend);

        // The shell over the same root: declare the type (the vocabulary
        // the cold read decodes against), register a unit, then relay.
        let host = Box::leak(Box::new(FoldHost {
            requires: Mutex::new(Vec::new()),
            state: json!({ "preset": "read-only" }),
        })) as &'static FoldHost;
        let exports = || HostExports {
            host_handle: std::ptr::from_ref(host)
                .cast::<std::os::raw::c_void>()
                .cast_mut(),
            require: fold_require,
            emit: ok_emit,
            grant: ok_grant,
        };
        let mut vocabulary = EventVocabulary::core();
        vocabulary
            .declare(&["permission/preset".to_owned()])
            .expect("declare");
        let link = super::HostLink::new(exports());
        let mut state = ShellState::with_parts(
            vocabulary,
            Projections::new(
                Arc::new(super::WireColdRead {
                    root: root.path().to_owned(),
                }),
                Arc::new(super::WireFoldCaller { link }),
            ),
        );

        let registration = json!({
            "key": "permissions",
            "stateVersion": 2,
            "fold": { "service": "permission-presets", "method": 2 },
        });
        dispatch(
            METHOD_PROJECTIONS_REGISTER,
            registration.to_string().as_bytes(),
            &mut state,
        )
        .expect("register");

        let reply = dispatch(
            METHOD_PROJECTIONS_STATE_OF,
            json!({ "id": "s-relay", "key": "permissions" })
                .to_string()
                .as_bytes(),
            &mut state,
        )
        .expect("relay");
        assert_eq!(
            serde_json::from_slice::<Value>(&reply).expect("json"),
            json!({ "state": { "preset": "read-only" } })
        );

        let requires = host.requires.lock().expect("lock");
        assert_eq!(requires.len(), 1, "exactly one fold call");
        assert_eq!(requires[0].0, "permission-presets");
        assert_eq!(requires[0].1, 2);
        assert_eq!(requires[0].2["events"][0]["type"], "permission/preset");
        assert_eq!(
            requires[0].2["events"][0]["data"],
            json!({ "preset": "read-only" })
        );
        assert_eq!(requires[0].2["header"]["id"], "s-relay");
        assert_eq!(requires[0].2["inheritedEventCount"], 0);
    }

    #[test]
    fn a_null_host_declines_the_activation() {
        assert!(ShellState::try_new(None).is_none());
    }

    #[test]
    fn malformed_declare_payloads_fail_with_text() {
        let mut state = shell_state();
        assert!(
            dispatch(METHOD_DECLARE_EVENT_TYPES, b"not json", &mut state)
                .expect_err("bad json")
                .contains("declare payload must be JSON")
        );
    }
}

// Rust guideline compliant 2026-10-29
