// Rust guideline compliant 2026-11-05
//! The plugin's outbound faces.
//!
//! The copied `HostExports` require seam (the fr-agent-loop `Outbound`
//! twin, require-only — a domain crate cannot link the loop) plus the
//! session-persistence client (declare/append/read; the fr-agent-loop
//! `SessionClient` judgment in minimal form).

use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_OK};

/// The copied vtable wrapper (per-activation lifetime: callable until
/// `teardown` returns; this plugin spawns no threads, so every call
/// happens on a host-invoked foreign entry's thread).
#[derive(Debug, Clone, Copy)]
pub struct Outbound(HostExports);

impl Outbound {
    /// Wraps the vtable copied at `setup`.
    #[must_use]
    pub const fn new(host: HostExports) -> Self {
        Self(host)
    }

    /// Performs one synchronous cross-plugin service call with the
    /// caller-owned growable buffer protocol (the host's documented
    /// 16 MiB ceiling; a `STATUS_CAPACITY` at that size is a protocol
    /// failure).
    ///
    /// # Errors
    ///
    /// The host status plus the JSON detail document the host delivered.
    pub fn require(
        &self,
        service: &str,
        method: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, (i32, String)> {
        let mut buffer = vec![0_u8; 4096];
        loop {
            let mut written: usize = 0;
            // SAFETY: the copied vtable is live for the activation; the
            // call runs on a host-invoked foreign entry's thread with an
            // empty foreign stack (no reentrancy).
            let status = unsafe {
                (self.0.require)(
                    self.0.host_handle,
                    service.as_ptr(),
                    service.len(),
                    method,
                    body.as_ptr(),
                    body.len(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &raw mut written,
                )
            };
            match status {
                STATUS_OK => {
                    buffer.truncate(written);
                    return Ok(buffer);
                }
                STATUS_CAPACITY => {
                    if buffer.len() >= MAX_REPLY_CAPACITY {
                        return Err((
                            status,
                            "out-buffer exhausted at the documented ceiling".to_owned(),
                        ));
                    }
                    buffer.resize(buffer.len() * 2, 0);
                }
                failed => {
                    let detail =
                        String::from_utf8_lossy(&buffer[..written.min(buffer.len())]).to_string();
                    return Err((failed, detail));
                }
            }
        }
    }
}

/// The host protocol's documented out-buffer ceiling.
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// The `session-persistence` client this plugin consumes (declare at
/// activation; append the two retry events; cold-read a session's log).
#[derive(Debug, Clone)]
pub struct PersistClient {
    outbound: Outbound,
    session: String,
}

impl PersistClient {
    /// One client over one outbound seam for one session.
    #[must_use]
    pub const fn new(outbound: Outbound, session: String) -> Self {
        Self { outbound, session }
    }

    /// Declares the durable event vocabulary (a duplicate fails closed
    /// at the session service; the fr-tools-builtin judgment).
    ///
    /// # Errors
    ///
    /// The transport or persistence refusal.
    pub fn declare(&self) -> Result<(), String> {
        self.outbound
            .require(
                fr_wire::services::session_persistence::SERVICE_NAME,
                fr_wire::services::session_persistence::METHOD_DECLARE_EVENT_TYPES,
                &serde_json::to_vec(&serde_json::json!({
                    "types": [
                        crate::wire::EVENT_LLM_RETRY,
                        crate::wire::EVENT_LLM_RETRY_STARTED,
                    ]
                }))
                .map_err(|error| format!("declare encode: {error}"))?,
            )
            .map(|_| ())
            .map_err(|(_, detail)| format!("declare: {detail}"))
    }

    /// Appends one typed event (the reply's committed envelope is
    /// dropped; the plugin only needs the durable fact).
    ///
    /// # Errors
    ///
    /// The transport or persistence refusal.
    pub fn append(&self, type_name: &str, data: &serde_json::Value) -> Result<(), String> {
        let request = serde_json::json!({
            "id": self.session,
            "type": type_name,
            "data": data,
        });
        self.outbound
            .require(
                fr_wire::services::session_persistence::SERVICE_NAME,
                fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
                &serde_json::to_vec(&request).map_err(|error| format!("append encode: {error}"))?,
            )
            .map(|_| ())
            .map_err(|(_, detail)| format!("append: {detail}"))
    }

    /// Cold-reads the session's log events (the `{"id"}` request; the
    /// reply's `events` array feeds the fold).
    ///
    /// # Errors
    ///
    /// The transport or persistence refusal.
    pub fn read_events(&self) -> Result<Vec<serde_json::Value>, String> {
        let reply = self
            .outbound
            .require(
                fr_wire::services::session_persistence::SERVICE_NAME,
                fr_wire::services::session_persistence::METHOD_SESSION_READ,
                &serde_json::to_vec(&serde_json::json!({ "id": self.session }))
                    .map_err(|error| format!("read encode: {error}"))?,
            )
            .map_err(|(_, detail)| format!("read: {detail}"))?;
        let reply: serde_json::Value =
            serde_json::from_slice(&reply).map_err(|error| format!("read decode: {error}"))?;
        reply
            .get("events")
            .and_then(serde_json::Value::as_array)
            .map_or_else(
                || Err("read reply carries no events array".to_owned()),
                |events| Ok(events.clone()),
            )
    }
}
