// Rust guideline compliant 2026-10-28
//! The C-ABI dylib plugin shell (PRD 4.5): fr-system-prompt's artifact
//! surface over the dougong dynamic channel.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer.
//! The shell provides the `system-prompt` service; `setup` stashes the
//! copied `HostExports` vtable so callback endpoints (Q86-A) resolve
//! through outbound `require`. The service is synchronous (the
//! fr-session append precedent: bounded local I/O on the service path),
//! so `teardown` only reclaims the state.

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};

use crate::instructions::config::InstructionConfig;
use crate::instructions::files::StdFs;
use crate::registry::RegistryConfig;
use crate::service::{PromptService, RequireSeam, ServiceError};

pub use crate::service::{
    METHOD_ASSEMBLE, METHOD_COMPOSE_INSTRUCTIONS, METHOD_REFRESH_CONTEXT, METHOD_REGISTER_CONTEXT,
    METHOD_REGISTER_SECTION, METHOD_REGISTER_TOOLS, METHOD_REGISTER_VARIABLE, METHOD_UNREGISTER,
    SERVICE_NAME,
};

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-system-prompt";

/// The descriptor the loader consumes: the `system-prompt` service over
/// the dynamic channel, no injected dependencies or listeners. The
/// version is sourced from crate metadata so it cannot drift.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-system-prompt\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"system-prompt\"],\"inject\":[],\"listen\":{}}"
)
.as_bytes();

/// The shell state: the service plus its copied require vtable.
pub struct ShellState {
    service: PromptService,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState")
            .field("service", &self.service)
            .finish()
    }
}

impl ShellState {
    /// Assembles the state over one copied vtable (a null host keeps the
    /// unattached resolver: endpoint providers then fail assembly).
    ///
    /// Returns `None` only when the default registry configuration fails
    /// (no `toolOrder`, so a build inconsistency); `setup` then declines
    /// activation with a null handle (the fr-session judgment).
    #[must_use]
    pub fn try_new(host: Option<RequireSeam>) -> Option<Self> {
        let mut service = PromptService::new(
            &RegistryConfig::default(),
            &InstructionConfig::base_patch(),
            Box::new(StdFs),
        )
        .ok()?;
        if let Some(seam) = host {
            service.attach_require_seam(seam);
        }
        Some(Self { service })
    }

    /// The hosted service (tests drive registrations and compose).
    #[must_use]
    pub const fn service(&self) -> &PromptService {
        &self.service
    }
}

/// One failed method call (the `{"error": ...}` `STATUS_FAILED` body).
#[derive(Debug)]
pub enum ShellFailure {
    /// The service failure text.
    Text(String),
}

/// Serves one method invocation against the shell state.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
pub fn dispatch(
    state: &mut ShellState,
    method: u32,
    payload: &[u8],
) -> Result<Vec<u8>, ShellFailure> {
    match crate::service::serve(&mut state.service, method, payload) {
        Ok(reply) => Ok(reply),
        Err(ServiceError(text)) => Err(ShellFailure::Text(text)),
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

// ABI v3 hands the outbound `HostExports` vtable here; the service copies
// it so callback endpoints resolve through outbound `require`.
unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    let seam = if host.is_null() {
        None
    } else {
        // SAFETY: the vtable contract guarantees validity for this call;
        // copying by value is the sanctioned pattern (dylib-plugin.md).
        Some(RequireSeam::new(unsafe { *host }))
    };
    let Some(state) = ShellState::try_new(seam) else {
        // Decline activation (null handle); the default configuration
        // failing is a build inconsistency, never a runtime input.
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
    // The host contract delivers `in_data` valid for `in_len` readable
    // bytes; an empty payload arrives as null/0 and never dereferences
    // the pointer, which `from_raw_parts` requires non-null even for a
    // zero-length slice.
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    // SAFETY: the handle was produced by `setup` and is valid until the
    // matching `teardown`, which the host never overlaps with a call; the
    // host serializes calls per the export-table contract, so the mutable
    // borrow is exclusive.
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
    let reply = match dispatch(state, method, payload) {
        Ok(reply) => reply,
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
    // The service owns no threads; reclaim the state.
    // SAFETY: the handle was produced by `setup` and torn down exactly
    // once.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}

/// Export table for the fr-system-prompt dylib artifact; the host
/// resolves this symbol by name at load time.
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
    use super::{METHOD_ASSEMBLE, METHOD_REGISTER_SECTION, PLUGIN_NAME, ShellState, dispatch};
    use crate::service::METHOD_DESCRIBE;

    #[test]
    fn describe_reports_the_shell_metadata() {
        let mut state = ShellState::try_new(None).expect("default configuration");
        let reply = dispatch(&mut state, METHOD_DESCRIBE, b"{}").expect("describe");
        let parsed: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["name"], "fr-system-prompt");
        assert_eq!(parsed["service"], "system-prompt");
        assert_eq!(parsed["abiVersion"], dougong_abi::ABI_VERSION);
    }

    #[test]
    fn descriptor_declares_the_system_prompt_service() {
        let parsed: serde_json::Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"][0], "system-prompt");
        assert_eq!(parsed["inject"], serde_json::json!([]));
        assert_eq!(parsed["listen"], serde_json::json!({}));
    }

    #[test]
    fn shell_assembles_and_fails_through_the_error_shape() {
        let mut state = ShellState::try_new(None).expect("default configuration");
        let reply = dispatch(&mut state, METHOD_ASSEMBLE, b"{}").expect("assemble");
        let parsed: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert!(
            parsed["sections"]
                .as_array()
                .is_some_and(|sections| !sections.is_empty())
        );
        let section = br#"{"name":"x","order":1,"text":"t"}"#;
        dispatch(&mut state, METHOD_REGISTER_SECTION, section).expect("register");
        let failure =
            dispatch(&mut state, METHOD_REGISTER_SECTION, section).expect_err("duplicate");
        assert!(
            matches!(failure, super::ShellFailure::Text(ref text) if text.contains("already registered")),
            "{failure:?}"
        );
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        let mut state = ShellState::try_new(None).expect("default configuration");
        let failure = dispatch(&mut state, 42, b"{}").expect_err("unknown");
        assert!(
            matches!(failure, super::ShellFailure::Text(ref text) if text.contains("unknown fr-system-prompt method 42")),
            "{failure:?}"
        );
    }
}
