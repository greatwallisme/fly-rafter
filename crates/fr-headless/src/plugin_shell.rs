//! The C-ABI dylib plugin shell: the `fr-headless` artifact surface over
//! the dougong dynamic channel.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer.

use std::sync::Mutex;

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};

/// The provided service name this plugin offers.
pub const PROVIDED_SERVICE: &str = "headless-startup";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-headless";

/// Local listen-table id: one `agent/status` flip (the decision-46
/// quiescence face the runner's whenIdle twin waits on).
pub const EV_STATUS: u32 = 1;
/// The event name behind [`EV_STATUS`].
pub const EVENT_STATUS: &str = "agent/status";
/// Local listen-table id: one `agent/error` boundary failure (the Q4B
/// failure tracker).
pub const EV_ERROR: u32 = 2;
/// The event name behind [`EV_ERROR`].
pub const EVENT_ERROR: &str = "agent/error";
/// Local listen-table id: one `agent/assistant-stream` bus batch (the
/// default-mode reasoning feed).
pub const EV_ASSISTANT_STREAM: u32 = 3;
/// The event name behind [`EV_ASSISTANT_STREAM`].
pub const EVENT_ASSISTANT_STREAM: &str = "agent/assistant-stream";
/// Local listen-table id: one `session/event` pump record (the `--json`
/// projection's feed and the failure cure).
pub const EV_SESSION_EVENT: u32 = 4;
/// The event name behind [`EV_SESSION_EVENT`].
pub const EVENT_SESSION_EVENT: &str = "session/event";

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 1;
/// Method selector: adopt the boot-window facts relay (the host's one call;
/// payload = the app facts document, stored verbatim).
pub const METHOD_ADOPT_FACTS: u32 = 2;
/// Method selector: read the stored facts snapshot back verbatim.
pub const METHOD_FACTS: u32 = 3;

/// The facts key the host bakes and relays (the app argv snapshot).
pub const FACTS_KEY: &str = "app";

/// The descriptor the loader consumes: the `headless-startup` service
/// provided, nothing injected, no listeners in this slice. The version is
/// sourced from crate metadata so it cannot drift from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-headless\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"headless-startup\"],\"inject\":[\"agent\",\
     \"session-persistence\",\"session-query\"],\"listen\":{\
     \"agent/status\":1,\"agent/error\":2,\"agent/assistant-stream\":3,\
     \"session/event\":4}}"
)
.as_bytes();

/// The shell state: the relayed facts snapshot plus the runner faces.
///
/// Fields: the facts mutex covers the adopt-then-read sequence across
/// the boundary's dispatch threads; the outbound seam serves the
/// runner's require/emit calls; the bus feeds the `on_event` fan-in;
/// the runner handle is joined by `teardown` (the runner mutex makes
/// the boot-window spawn idempotent).
#[derive(Debug)]
pub struct ShellState {
    facts: Mutex<Option<serde_json::Value>>,
    outbound: fr_agent_loop::outbound::Outbound,
    bus: std::sync::Arc<crate::runner::RunnerBus>,
    runner: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ShellState {
    /// An empty shell over one outbound seam: no facts relayed yet (the
    /// pre-relay arm), no runner spawned.
    #[must_use]
    pub fn new(outbound: fr_agent_loop::outbound::Outbound) -> Self {
        Self {
            facts: Mutex::new(None),
            outbound,
            bus: std::sync::Arc::new(crate::runner::RunnerBus::new()),
            runner: Mutex::new(None),
        }
    }

    /// The bus the runner thread consumes (the `on_event` fan-in target).
    #[must_use]
    pub const fn bus(&self) -> &std::sync::Arc<crate::runner::RunnerBus> {
        &self.bus
    }

    /// Spawns the runner thread exactly once, over the relayed facts.
    /// The `ADOPT_FACTS` arm calls this after adopting, so the facts are
    /// the snapshot the host relayed in the boot window.
    fn spawn_runner_once(&self) {
        let mut runner = self.runner.lock().expect("runner lock poisoned");
        if runner.is_some() {
            return;
        }
        let facts = self
            .facts
            .lock()
            .expect("facts lock poisoned")
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "app": { "argv": [] } }));
        let argv: Vec<String> = facts
            .get("app")
            .and_then(|app| app.get("argv"))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let deps = crate::runner::RunnerDeps {
            outbound: self.outbound,
            bus: std::sync::Arc::clone(&self.bus),
        };
        // The thread owns its argv/facts copies and the deps; the model
        // selection rides the facts' `model` section (the runner reads
        // both). `teardown` joins the handle before the state (and the
        // copied vtable) is released (the v3 thread discipline).
        let spawned = std::thread::Builder::new()
            .name("fr-headless:runner".to_owned())
            .spawn(move || crate::runner::run_stdio(&argv, &facts, &deps));
        match spawned {
            Ok(handle) => *runner = Some(handle),
            // A thread-spawn failure is an exhaustion-level condition; the
            // run cannot start, so the failure is loud on stderr (the
            // plugin's only console face) and the process stays with the
            // host's signal face.
            Err(error) => eprintln!("fr: headless: runner spawn failed: {error}"),
        }
    }

    /// Stores the facts snapshot (wholesale replace, the adopt semantics).
    ///
    /// # Errors
    ///
    /// Surfaces the lock-poison error text; the Result keeps the dispatch
    /// face total.
    pub fn adopt(&self, facts: &serde_json::Value) -> Result<(), String> {
        match self.facts.lock() {
            Ok(mut slot) => {
                *slot = Some(facts.clone());
                Ok(())
            }
            Err(error) => Err(format!("fr-headless facts mutex poisoned: {error}")),
        }
    }

    /// The stored snapshot, verbatim; `{"app": {"argv": []}}` before any
    /// relay (the empty-composition arm).
    ///
    /// # Errors
    ///
    /// Surfaces the lock-poison error text; the Result keeps the dispatch
    /// face total.
    pub fn facts(&self) -> Result<serde_json::Value, String> {
        let slot = match self.facts.lock() {
            Ok(slot) => slot,
            Err(error) => return Err(format!("fr-headless facts mutex poisoned: {error}")),
        };
        Ok(slot
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "app": { "argv": [] } })))
    }
}

/// Serves one method invocation against the shell state: a pure
/// (JSON-in, JSON-out) function over `&ShellState` so the surface stays
/// testable without the boundary.
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`.
pub fn dispatch(state: &ShellState, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
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
        METHOD_ADOPT_FACTS => {
            let facts: serde_json::Value = serde_json::from_slice(payload)
                .map_err(|error| format!("facts document decode: {error}"))?;
            state.adopt(&facts)?;
            Ok(serde_json::to_vec(&serde_json::json!({ "adopted": true })).unwrap_or_default())
        }
        METHOD_FACTS => {
            let _ = payload;
            Ok(serde_json::to_vec(&state.facts()?).unwrap_or_default())
        }
        _ => Err(format!("unknown fr-headless method {method}")),
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

// ABI v3 hands the outbound `HostExports` vtable here. Like the loop
// shell, this plugin consumes outbound calls (the runner's requires and
// the exit emit), so the vtable is copied by value and retained for the
// activation's lifetime; the runner thread holding a copy is joined
// before `teardown` returns (v3 thread discipline).
unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    if host.is_null() {
        // Without the vtable the runner cannot reach any consumed service
        // or request the exit: fail the activation loudly.
        return std::ptr::null_mut();
    }
    // SAFETY: the host exports pointer is valid for the duration of
    // `setup`; the vtable is copied by value (v3 contract).
    let host = unsafe { *host };
    let outbound = fr_agent_loop::outbound::Outbound::new(host);
    Box::into_raw(Box::new(ShellState::new(outbound))).cast::<std::ffi::c_void>()
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
    // SAFETY: the handle was produced by `setup` and stays alive until
    // `teardown` reclaims it (export-table contract).
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    // The host contract delivers `in_data` valid for `in_len` readable
    // bytes; an empty payload arrives as null/0 and never dereferences the
    // pointer, which `from_raw_parts` requires non-null even for a
    // zero-length slice.
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    let reply = match dispatch(state, method, payload) {
        Ok(reply) => reply,
        Err(message) => {
            let body = serde_json::json!({ "error": message }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            return STATUS_FAILED;
        }
    };
    // The boot-window relay is the runner's start gun: the facts are in,
    // the composition is joined, so the runner thread spawns exactly once
    // and drives the one-shot run to the exit request.
    if method == METHOD_ADOPT_FACTS {
        state.spawn_runner_once();
    }
    // SAFETY: host buffer contract.
    if unsafe { write_out(&reply, out, cap, out_len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

// The four listened events (all emit dispatch): decode the payload and
// push onto the runner bus — no `require`, no blocking (the on-event
// fast-path invariant, decision 46).
unsafe extern "C" fn on_event(
    handle: *mut std::os::raw::c_void,
    event: u32,
    _mode: u32,
    in_data: *const u8,
    in_len: usize,
    _next_available: u32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // SAFETY: live handle per the export-table contract.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    // Same empty-payload shape as `call` (null/0 pairs).
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    let decoded = match event {
        EV_STATUS => crate::runner::decode_status(payload),
        EV_ERROR => crate::runner::decode_failure(payload),
        EV_ASSISTANT_STREAM => crate::runner::decode_frames(payload),
        EV_SESSION_EVENT => crate::runner::decode_session_event(payload),
        _ => Err(format!("unknown fr-headless listened event {event}")),
    };
    match decoded {
        Ok(event) => {
            state.bus().push(event);
            // SAFETY: host buffer contract; an empty JSON object reply.
            if unsafe { write_out(b"{}", out, cap, out_len) } {
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
    let state = unsafe { Box::from_raw(handle.cast::<ShellState>()) };
    // Wake-before-join: the runner parks on composition events, so the
    // bus closes first (the coordinated shutdown — the dsh early-dispose
    // return twin) and every wait ends; a runner mid-require fails its
    // call against the disposing tree and unwinds to the same close.
    state.bus().close();
    // The runner thread holds a vtable copy: join it before the state
    // (and the copied vtable) is released (v3 thread discipline). A
    // panicked runner is already loud; its join result is dropped rather
    // than re-panicking through teardown.
    if let Ok(mut runner) = state.runner.lock()
        && let Some(handle) = runner.take()
    {
        let _ = handle.join();
    }
}

/// Export table for the fr-headless dylib artifact; the host resolves this
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

/// A quiet outbound seam for the pure-dispatch tests: no call ever
/// reaches it (the shell face never invokes consumed services on the
/// dispatch path).
#[cfg(test)]
fn quiet_outbound() -> fr_agent_loop::outbound::Outbound {
    unsafe extern "C" fn failing_require(
        _host: *mut std::ffi::c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        _in_data: *const u8,
        _in_len: usize,
        _out: *mut u8,
        _cap: usize,
        _out_len: *mut usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
    }
    unsafe extern "C" fn quiet_emit(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_OK
    }
    unsafe extern "C" fn quiet_grant(
        _host: *mut std::ffi::c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_OK
    }
    unsafe extern "C" fn noop_dispatch(
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
    fr_agent_loop::outbound::Outbound::new(HostExports {
        grant: quiet_grant,
        dispatch: noop_dispatch,
        host_handle: std::ptr::null_mut(),
        require: failing_require,
        emit: quiet_emit,
        realm_spawn: quiet_realm_spawn,
        realm_retire: quiet_realm_retire,
        realm_require: quiet_realm_require,
    })
}

/// Test double for the host side of the ABI v6 `realm_spawn` entry (D33):
/// no headless test drives a realm activation, so the double accepts quietly.
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
/// no headless test drives a realm retirement, so the double accepts quietly.
#[cfg(test)]
pub(crate) const unsafe extern "C" fn quiet_realm_retire(
    _host: *mut std::ffi::c_void,
    _child: u64,
) -> i32 {
    dougong_abi::STATUS_OK
}

/// Test double for the host side of the ABI v6 `realm_require` entry (D33):
/// no headless test drives a realm call, so the double accepts quietly.
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
    use super::{
        METHOD_ADOPT_FACTS, METHOD_DESCRIBE, METHOD_FACTS, PLUGIN_NAME, PROVIDED_SERVICE,
        ShellState, dispatch, quiet_outbound,
    };

    #[test]
    fn describe_reports_the_shell_metadata() {
        let state = ShellState::new(quiet_outbound());
        let reply = dispatch(&state, METHOD_DESCRIBE, b"{}").expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], PLUGIN_NAME);
        assert_eq!(reply["provides"], PROVIDED_SERVICE);
        assert_eq!(reply["abiVersion"], dougong_abi::ABI_VERSION);
    }

    #[test]
    fn adopt_then_read_returns_the_snapshot_verbatim() {
        let state = ShellState::new(quiet_outbound());
        let facts = serde_json::json!({ "app": { "argv": ["--json", "run it"] } });
        let payload = serde_json::to_vec(&facts).expect("facts json");
        dispatch(&state, METHOD_ADOPT_FACTS, &payload).expect("adopt");
        let echo = dispatch(&state, METHOD_FACTS, b"{}").expect("facts");
        assert_eq!(echo, payload);
    }

    #[test]
    fn facts_before_any_relay_report_the_empty_composition() {
        let state = ShellState::new(quiet_outbound());
        let echo = dispatch(&state, METHOD_FACTS, b"{}").expect("facts");
        let echo: serde_json::Value = serde_json::from_slice(&echo).expect("json");
        assert_eq!(echo["app"]["argv"], serde_json::json!([]));
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        let state = ShellState::new(quiet_outbound());
        assert!(
            dispatch(&state, 9, b"{}")
                .expect_err("unknown method")
                .contains("unknown fr-headless method 9")
        );
    }

    #[test]
    fn descriptor_declares_the_runner_surface() {
        // Parses the shipped `DESCRIPTOR` constant itself, so any drift (for
        // example a stale embedded version) fails this test.
        let parsed: serde_json::Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"], serde_json::json!(["headless-startup"]));
        // The Q2A ruling: the runner consumes the agent, persistence, and
        // query faces (the dsh runner inject twin).
        assert_eq!(
            parsed["inject"],
            serde_json::json!(["agent", "session-persistence", "session-query"])
        );
        // The four listened events with their local ids (the on_event
        // fan-in table).
        assert_eq!(
            parsed["listen"],
            serde_json::json!({
                "agent/status": 1,
                "agent/error": 2,
                "agent/assistant-stream": 3,
                "session/event": 4,
            })
        );
    }
}

// Rust guideline compliant 2026-11-04
