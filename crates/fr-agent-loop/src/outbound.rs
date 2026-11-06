// Rust guideline compliant 2026-10-27
//! The typed outbound layer over the dougong ABI v3 `HostExports` vtable.
//!
//! `setup` copies the vtable by value (per-activation lifetime: callable
//! until `teardown` returns). Each loop thread owns an [`Outbound`] copy —
//! the struct is `Copy` and `Send` because the vtable contract allows
//! `require` and `emit` from any plugin thread, concurrently.
//!
//! `require` follows the caller-owned growable buffer protocol: a
//! `STATUS_CAPACITY` reply doubles the buffer and retries, bounded at the
//! 16 MiB ceiling the host protocol documents. `emit` publishes one event
//! (valid JSON payload, non-`internal/*` name) enqueue-and-return — the
//! bus promises no ordering between two emits or between an emit and a
//! require (dougong dylib-plugin.md:174-176), which is exactly why stream
//! consumption is notify-plus-pull (Q84-B2) rather than payload-carrying
//! events.

use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_OK};

/// The waterfall dispatch mode (the kernel bus's compose-around-a-
/// continuation semantics; re-exported so the driver's kernel-family
/// edge stays this module).
pub use dougong_abi::MODE_WATERFALL as WATERFALL_MODE;

/// The host protocol's documented out-buffer ceiling (16 MiB); a reply
/// that still reports `STATUS_CAPACITY` at this size is a protocol
/// failure, not a reason to grow further.
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// A failed outbound `require`: the raw host status plus the JSON detail
/// document the host delivered in the out buffer (empty when the host sent
/// none).
#[derive(Debug)]
pub struct RequireError {
    /// The ABI status code (`STATUS_UNDECLARED`, `STATUS_FAILED`, ...).
    pub status: i32,
    /// The host's JSON detail document, when present.
    pub detail: Vec<u8>,
}

impl std::fmt::Display for RequireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "outbound require failed with host status {}",
            self.status
        )?;
        if !self.detail.is_empty() {
            write!(f, ": {}", String::from_utf8_lossy(&self.detail))?;
        }
        Ok(())
    }
}

impl std::error::Error for RequireError {}

/// A failed outbound `emit`.
#[derive(Debug)]
pub struct EmitError {
    /// The ABI status code (`STATUS_INVALID_PAYLOAD`, ...).
    pub status: i32,
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "outbound emit failed with host status {}", self.status)
    }
}

impl std::error::Error for EmitError {}

/// A failed outbound `dispatch`: the raw host status plus the JSON detail
/// document the host delivered in the out buffer.
///
/// A listener failure's detail is the `{"code","class","message"}`
/// triple, empty when the host sent none.
#[derive(Debug)]
pub struct DispatchError {
    /// The ABI status code (`STATUS_FAILED`, `STATUS_REENTRANCY`, ...).
    pub status: i32,
    /// The host's JSON detail document, when present.
    pub detail: Vec<u8>,
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "outbound dispatch failed with host status {}",
            self.status
        )?;
        if !self.detail.is_empty() {
            write!(f, ": {}", String::from_utf8_lossy(&self.detail))?;
        }
        Ok(())
    }
}

impl std::error::Error for DispatchError {}

/// One copied vtable: handed to each loop thread, which then performs its
/// own outbound calls (PRD decision 46, Q79-A).
#[derive(Debug, Clone, Copy)]
pub struct Outbound {
    host: HostExports,
}

// SAFETY: the ABI vtable contract states both entries are callable from
// any plugin thread, concurrently, with host-side panic containment; the
// copy stays valid until this activation's `teardown` returns and the
// shell joins every thread holding a copy before then.
unsafe impl Send for Outbound {}

// SAFETY: sharing one copy across threads only ever issues concurrent
// vtable calls, which the same contract explicitly allows.
unsafe impl Sync for Outbound {}

impl Outbound {
    /// Wraps a vtable copied at `setup`.
    #[must_use]
    pub const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Performs one synchronous, blocking cross-plugin service call.
    ///
    /// Retries with a doubled buffer while the host reports
    /// [`STATUS_CAPACITY`] (the growable buffer protocol), up to the
    /// 16 MiB ceiling the host protocol documents.
    ///
    /// # Errors
    ///
    /// Returns the host status and any JSON detail document on any
    /// non-OK terminal status.
    pub fn require(
        &self,
        service: &str,
        method: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, RequireError> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: the copied vtable is live until this activation's
            // `teardown` returns; every caller (a loop thread) is joined
            // before `teardown` (shell thread discipline). Buffers are
            // caller-owned per the export-table contract.
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
                    return Err(RequireError {
                        status,
                        detail: format!("reply still exceeds {MAX_REPLY_CAPACITY} bytes")
                            .into_bytes(),
                    });
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            return Err(RequireError {
                status,
                detail: buf,
            });
        }
    }

    /// Publishes one event to the kernel bus (enqueue-and-return; the bus
    /// promises no ordering).
    ///
    /// # Errors
    ///
    /// Returns the host status for an invalid payload, an `internal/*`
    /// event name, or any other host rejection.
    pub fn emit(&self, event: &str, payload: &[u8]) -> Result<(), EmitError> {
        // SAFETY: same vtable lifetime and thread discipline as `require`.
        let status = unsafe {
            (self.host.emit)(
                self.host.host_handle,
                event.as_ptr(),
                event.len(),
                payload.as_ptr(),
                payload.len(),
            )
        };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(EmitError { status })
        }
    }

    /// Performs one synchronous five-mode kernel dispatch and returns
    /// the settled chain value (ABI v5, D32; the `agent/pre-step` and
    /// `agent/request-error` waterfalls' outbound face).
    ///
    /// Await modes block the calling thread until the chain settles and
    /// follow the same growable `out_buf` protocol as [`Self::require`]:
    /// an untouched waterfall chain reports the original payload bytes
    /// (the identity continuation), and a listener failure is
    /// `STATUS_FAILED` with the JSON `{"code","class","message"}` detail.
    /// The call must originate on a plugin-owned thread — the loop's
    /// driver thread is exactly that (the shell spawns it), so every
    /// driver-side dispatch is legal and any foreign-entry call fails
    /// loudly instead of hanging.
    ///
    /// # Errors
    ///
    /// Returns the host status and any JSON detail document on any
    /// non-OK terminal status.
    pub fn dispatch(
        &self,
        event: &str,
        mode: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, DispatchError> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: the copied vtable is live until this activation's
            // `teardown` returns; every caller (a driver thread) is joined
            // before `teardown` (shell thread discipline). Buffers are
            // caller-owned per the export-table contract.
            let status = unsafe {
                (self.host.dispatch)(
                    self.host.host_handle,
                    event.as_ptr(),
                    event.len(),
                    mode,
                    payload.as_ptr(),
                    payload.len(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut len,
                )
            };
            if status == STATUS_CAPACITY {
                if cap >= MAX_REPLY_CAPACITY {
                    return Err(DispatchError {
                        status,
                        detail: format!("reply still exceeds {MAX_REPLY_CAPACITY} bytes")
                            .into_bytes(),
                    });
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            return Err(DispatchError {
                status,
                detail: buf,
            });
        }
    }

    /// Grants one committed target activation runtime access to the
    /// `'agent'`-class provided service (the D31 registration primitive;
    /// the fr-shell `HostLink::grant` judgment shape).
    ///
    /// # Errors
    ///
    /// Returns the host status for an uncommitted target or a name not
    /// in this activation's provide table.
    pub fn grant(&self, target_service: &str, granted_service: &str) -> Result<(), GrantError> {
        // SAFETY: same vtable lifetime and thread discipline as `require`.
        let status = unsafe {
            (self.host.grant)(
                self.host.host_handle,
                target_service.as_ptr(),
                target_service.len(),
                granted_service.as_ptr(),
                granted_service.len(),
            )
        };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(GrantError { status })
        }
    }

    /// Activates one child plugin artifact inside a realm owned by this
    /// activation (ABI v6, D33; the 18e `fiber_realm` ruling's spawn face).
    ///
    /// `realm == 0` allocates a fresh realm label; a non-zero realm must be
    /// one an earlier spawn returned. The call blocks the calling thread
    /// until the child settles and replies with the JSON spawn document
    /// (`{"realm":L,"child":C,"state":"active"|"pending"}`). The out-buffer
    /// honors [`dougong_abi::REALM_SPAWN_REPLY_MIN`] up front so the host
    /// never answers `STATUS_CAPACITY`.
    ///
    /// The call must originate on a plugin-owned thread (a foreign entry
    /// frame fails with `STATUS_REENTRANCY`); the parent lifecycle arms run
    /// the spawn on a scope-spawned worker and join it inside the service
    /// entry — safe because the child's setup and adopt handshake touch
    /// only global services, never this activation's `agent` face (the
    /// realm-partner judgment, documented at the call site).
    ///
    /// # Errors
    ///
    /// Returns the host status and any JSON detail document on any
    /// non-OK terminal status (`realm_spawn_load_failed` for an unloadable
    /// artifact, the child's own startup error otherwise).
    pub fn realm_spawn(&self, realm: u64, path: &str) -> Result<Vec<u8>, RequireError> {
        let mut buf = vec![0_u8; 4096_usize.max(dougong_abi::REALM_SPAWN_REPLY_MIN)];
        let mut len = 0_usize;
        // SAFETY: the copied vtable is live until this activation's
        // `teardown` returns; every caller is joined before `teardown`
        // (shell thread discipline). Buffers are caller-owned per the
        // export-table contract.
        let status = unsafe {
            (self.host.realm_spawn)(
                self.host.host_handle,
                realm,
                path.as_ptr(),
                path.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut len,
            )
        };
        buf.truncate(len);
        if status == STATUS_OK {
            Ok(buf)
        } else {
            Err(RequireError {
                status,
                detail: buf,
            })
        }
    }

    /// Disposes one child this activation spawned (ABI v6, D33) and
    /// returns once the child's teardown has settled. Status-only like
    /// [`Self::grant`]; the failure cleanup leg of the parent lifecycle
    /// arms ignores a refusal rather than masking the original error.
    ///
    /// # Errors
    ///
    /// Returns the host status for an unknown or already-retired handle.
    pub fn realm_retire(&self, child: u64) -> Result<(), RequireError> {
        // SAFETY: same vtable lifetime and thread discipline as `require`.
        let status = unsafe { (self.host.realm_retire)(self.host.host_handle, child) };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(RequireError {
                status,
                detail: Vec::new(),
            })
        }
    }

    /// `require` addressed to a realm (ABI v6, D33): resolves `service` in
    /// the named realm and calls `method`, authorizing the call only when
    /// the provider is a child this activation spawned. The parent routes
    /// the session lifecycle and session methods into the session realm
    /// through this face with the original method numbers.
    ///
    /// Follows the same growable buffer protocol as [`Self::require`].
    ///
    /// # Errors
    ///
    /// Returns the host status and any JSON detail document on any
    /// non-OK terminal status (`unknown_realm` / `STATUS_UNDECLARED` for
    /// a provider that is not this activation's child).
    pub fn realm_require(
        &self,
        realm: u64,
        service: &str,
        method: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, RequireError> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: the copied vtable is live until this activation's
            // `teardown` returns; every caller is joined before `teardown`
            // (shell thread discipline). Buffers are caller-owned per the
            // export-table contract.
            let status = unsafe {
                (self.host.realm_require)(
                    self.host.host_handle,
                    realm,
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
                    return Err(RequireError {
                        status,
                        detail: format!("reply still exceeds {MAX_REPLY_CAPACITY} bytes")
                            .into_bytes(),
                    });
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            return Err(RequireError {
                status,
                detail: buf,
            });
        }
    }
}

/// The failure of one D31 grant registration (the host status only; the
/// grant performs no call and carries no JSON detail).
#[derive(Debug, Clone, Copy)]
pub struct GrantError {
    /// The host status the grant returned.
    pub status: i32,
}

/// Test double for the host side of `grant` (the kernel validates grant
/// semantics under its own tests; the loop's doubles only accept).
#[cfg(test)]
pub(crate) const unsafe extern "C" fn quiet_grant(
    _host: *mut std::ffi::c_void,
    _target: *const u8,
    _target_len: usize,
    _granted: *const u8,
    _granted_len: usize,
) -> i32 {
    dougong_abi::STATUS_OK
}

/// Test double for the host side of `dispatch` (D32): the untouched
/// chain's identity continuation — the reply echoes the payload bytes —
/// so the default double dispatch behaves like a bus with no listeners.
#[cfg(test)]
pub(crate) const unsafe extern "C" fn identity_dispatch(
    _host: *mut std::ffi::c_void,
    _event: *const u8,
    _event_len: usize,
    _mode: u32,
    payload: *const u8,
    payload_len: usize,
    out_buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if payload_len > cap {
        return dougong_abi::STATUS_CAPACITY;
    }
    // SAFETY: the vtable contract passes one valid readable payload and a
    // `cap`-sized out-buffer with a writable `len` slot.
    unsafe {
        std::ptr::copy_nonoverlapping(payload, out_buf, payload_len);
        *out_len = payload_len;
    }
    dougong_abi::STATUS_OK
}

/// Test double for the host side of `dispatch` (D32): every call fails
/// loudly, for tests that must observe no outbound dispatch at all.
#[cfg(test)]
pub(crate) const unsafe extern "C" fn noop_dispatch(
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

/// Test double for the host side of the ABI v6 `realm_spawn` entry (D33):
/// no loop test drives a realm activation, so the double accepts quietly.
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
/// no loop test drives a realm retirement, so the double accepts quietly.
#[cfg(test)]
pub(crate) const unsafe extern "C" fn quiet_realm_retire(
    _host: *mut std::ffi::c_void,
    _child: u64,
) -> i32 {
    dougong_abi::STATUS_OK
}

/// Test double for the host side of the ABI v6 `realm_require` entry (D33):
/// no loop test drives a realm call, so the double accepts quietly.
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
    use super::{Outbound, RequireError};
    use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_OK, STATUS_UNDECLARED};
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test double for the host side of `require`: the first call reports
    /// `STATUS_CAPACITY` regardless of the offered buffer, later calls
    /// copy `reply` and succeed.
    struct CapacityFake {
        calls: AtomicUsize,
        reply: Vec<u8>,
    }

    unsafe extern "C" fn capacity_require(
        host: *mut c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        _in_data: *const u8,
        _in_len: usize,
        out_buf: *mut u8,
        _cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: `host` is the `CapacityFake` pointer this test installed
        // as the vtable's host handle.
        let fake = unsafe { &*host.cast::<CapacityFake>() };
        if fake.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            return STATUS_CAPACITY;
        }
        // SAFETY: the caller (the `require` wrapper under test) offers a
        // buffer sized for `reply` after the doubling retry; `out_len` is
        // a writable slot per the buffer protocol.
        unsafe {
            std::ptr::copy_nonoverlapping(fake.reply.as_ptr(), out_buf, fake.reply.len());
            *out_len = fake.reply.len();
        }
        STATUS_OK
    }

    /// Test double for the host side of `require`: always fails with
    /// `STATUS_UNDECLARED` and a JSON detail document (no state needed —
    /// the host handle is ignored).
    unsafe extern "C" fn undeclared_require(
        _host: *mut c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        _in_data: *const u8,
        _in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        let detail = br#"{"error":"service not declared"}"#;
        // SAFETY: buffer protocol as in `capacity_require`.
        if unsafe { dougong_abi::write_out(detail, out_buf, cap, out_len) } {
            STATUS_UNDECLARED
        } else {
            STATUS_CAPACITY
        }
    }

    /// Test double for the host side of `emit`: records name and payload.
    struct BusFake {
        events: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
    }

    unsafe extern "C" fn recording_emit(
        host: *mut c_void,
        event: *const u8,
        event_len: usize,
        payload: *const u8,
        payload_len: usize,
    ) -> i32 {
        // SAFETY: `host` is the `BusFake` pointer installed by the test.
        let bus = unsafe { &*host.cast::<BusFake>() };
        // SAFETY: the wrapper under test passes slices valid for their
        // lengths per the vtable contract.
        let event = unsafe { std::slice::from_raw_parts(event, event_len) }.to_vec();
        // SAFETY: same slice-validity contract as `event` above.
        let payload = unsafe { std::slice::from_raw_parts(payload, payload_len) }.to_vec();
        bus.events
            .lock()
            .expect("test bus lock")
            .push((event, payload));
        STATUS_OK
    }

    /// Test double for the host side of `grant`: records the two names
    /// verbatim (the notice machine's registration face).
    struct GrantFake {
        grants: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
    }

    unsafe extern "C" fn recording_grant(
        host: *mut c_void,
        target: *const u8,
        target_len: usize,
        granted: *const u8,
        granted_len: usize,
    ) -> i32 {
        // SAFETY: `host` is the `GrantFake` pointer installed by the test.
        let fake = unsafe { &*host.cast::<GrantFake>() };
        // SAFETY: slice-validity contract as in `recording_emit`.
        let target = unsafe { std::slice::from_raw_parts(target, target_len) }.to_vec();
        // SAFETY: same contract as `target`.
        let granted = unsafe { std::slice::from_raw_parts(granted, granted_len) }.to_vec();
        fake.grants
            .lock()
            .expect("test grant lock")
            .push((target, granted));
        STATUS_OK
    }

    /// Test double for the host side of `grant`: always refuses (the
    /// uncommitted-target shape).
    unsafe extern "C" fn failed_grant(
        _host: *mut c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
    }

    /// Test double for the host side of `emit` used by tests that never
    /// publish: accepts and does nothing.
    unsafe extern "C" fn quiet_emit(
        _host: *mut c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    unsafe extern "C" fn rejecting_emit(
        _host: *mut c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_INVALID_PAYLOAD
    }

    fn outbound_with(
        host_handle: *mut c_void,
        require: unsafe extern "C" fn(
            *mut c_void,
            *const u8,
            usize,
            u32,
            *const u8,
            usize,
            *mut u8,
            usize,
            *mut usize,
        ) -> i32,
        emit: unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize) -> i32,
    ) -> Outbound {
        Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            dispatch: crate::outbound::noop_dispatch,
            host_handle,
            require,
            emit,
            realm_spawn: crate::outbound::quiet_realm_spawn,
            realm_retire: crate::outbound::quiet_realm_retire,
            realm_require: crate::outbound::quiet_realm_require,
        })
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn grant_passes_both_names_and_reports_failure() {
        let fake = GrantFake {
            grants: Mutex::new(Vec::new()),
        };
        let outbound = Outbound::new(HostExports {
            grant: recording_grant,
            dispatch: crate::outbound::noop_dispatch,
            host_handle: &raw const fake as *mut c_void,
            require: undeclared_require,
            emit: quiet_emit,
            realm_spawn: crate::outbound::quiet_realm_spawn,
            realm_retire: crate::outbound::quiet_realm_retire,
            realm_require: crate::outbound::quiet_realm_require,
        });
        outbound
            .grant("builtin-tools", "agent")
            .expect("the grant registers");
        assert_eq!(
            fake.grants.lock().expect("test grant lock").as_slice(),
            [(b"builtin-tools".to_vec(), b"agent".to_vec())]
        );

        let refusing = Outbound::new(HostExports {
            grant: failed_grant,
            dispatch: crate::outbound::noop_dispatch,
            host_handle: std::ptr::null_mut(),
            require: undeclared_require,
            emit: quiet_emit,
            realm_spawn: crate::outbound::quiet_realm_spawn,
            realm_retire: crate::outbound::quiet_realm_retire,
            realm_require: crate::outbound::quiet_realm_require,
        });
        let error = refusing
            .grant("builtin-tools", "agent")
            .expect_err("uncommitted target");
        assert_eq!(error.status, dougong_abi::STATUS_FAILED);
    }

    #[test]
    fn require_doubles_the_buffer_until_the_reply_fits() {
        assert_send::<Outbound>();
        let fake = CapacityFake {
            calls: AtomicUsize::new(0),
            reply: vec![b'x'; 5000],
        };
        let outbound = outbound_with(&raw const fake as *mut c_void, capacity_require, quiet_emit);
        let reply = outbound
            .require("llm", 7, b"payload")
            .expect("reply fits after doubling");
        assert_eq!(reply, fake.reply);
        // One CAPACITY round-trip (4096 too small) then one success.
        assert_eq!(fake.calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn require_error_carries_status_and_detail() {
        let outbound = outbound_with(std::ptr::null_mut(), undeclared_require, quiet_emit);
        let error: RequireError = outbound
            .require("llm", 0, b"{}")
            .expect_err("undeclared service");
        assert_eq!(error.status, STATUS_UNDECLARED);
        assert_eq!(error.detail, br#"{"error":"service not declared"}"#);
        assert!(error.to_string().contains("host status -3"));
        assert!(error.to_string().contains("service not declared"));
    }

    #[test]
    fn emit_passes_name_and_payload_and_reports_rejection() {
        let bus = BusFake {
            events: Mutex::new(Vec::new()),
        };
        let outbound = outbound_with(
            &raw const bus as *mut c_void,
            undeclared_require,
            recording_emit,
        );
        outbound
            .emit("agent/status", br#"{"phase":"running"}"#)
            .expect("emit accepted");
        let events = bus.events.lock().expect("test bus lock");
        assert_eq!(
            events.as_slice(),
            [(b"agent/status".to_vec(), br#"{"phase":"running"}"#.to_vec())]
        );
        drop(events);

        let rejecting = outbound_with(std::ptr::null_mut(), undeclared_require, rejecting_emit);
        let error = rejecting
            .emit("agent/status", b"not json")
            .expect_err("invalid payload");
        assert_eq!(error.status, dougong_abi::STATUS_INVALID_PAYLOAD);
    }
}
