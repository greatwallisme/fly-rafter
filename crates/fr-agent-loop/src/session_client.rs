// Rust guideline compliant 2026-10-27
//! The consumer-side session-persistence wrappers.
//!
//! One thin client over the dougong dynamic channel: the loop encodes
//! dsh-shaped method payloads, performs a synchronous `require`, and
//! decodes the reply's committed envelope. Method numbers and payload
//! shapes are the provider contract in `crates/fr-session/src/service.rs`
//! (create/append/flush/fork/resume/restore/drain, methods 2-8); the
//! service-name wiring lands with the Phase 18 assembly, so the inject
//! pin here follows PRD 8.1 (`session-persistence`).
//!
//! The append reply carries the committed event — seq and time assigned
//! provider-side — which is the fold authority for every loop-owned
//! projection (inbox, turn boundary): replaying the committed event keeps
//! the live path and the restore path on one source of truth.

use std::error::Error;
use std::fmt;

use fr_wire::event::{SessionEvent, SurfaceOp};
use fr_wire::ids::SessionSeq;

use crate::outbound::{Outbound, RequireError};

/// The injected session service (PRD 8.1 inject face).
pub const SERVICE_NAME: &str = "session-persistence";

/// Method selector: append one typed event
/// (`{id, type, data, surfaceOp?, sourceEventSeqs?}`; the provider face is
/// `fr_session::service::METHOD_SESSION_APPEND`).
pub const METHOD_APPEND: u32 = 3;

/// One failed session-service call.
#[derive(Debug)]
pub enum SessionClientError {
    /// The host or provider rejected the call.
    Transport(RequireError),
    /// The reply did not match the method's contract.
    Reply(String),
}

impl fmt::Display for SessionClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "session service call failed: {error}"),
            Self::Reply(text) => write!(f, "session service reply is invalid: {text}"),
        }
    }
}

impl Error for SessionClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Reply(_) => None,
        }
    }
}

/// The `session.append` request body (declaration order is the wire order).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppendRequest<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    type_name: &'a str,
    data: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    surface_op: Option<&'a SurfaceOp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_event_seqs: Option<&'a [SessionSeq]>,
}

/// The `session.append` reply body.
#[derive(serde::Deserialize)]
struct AppendReply {
    event: SessionEvent,
}

/// One session's outbound session-service handle.
#[derive(Debug, Clone)]
pub struct SessionClient {
    outbound: Outbound,
    session_id: String,
}

impl SessionClient {
    /// Builds the client over the activation's outbound seam.
    #[must_use]
    pub fn new(outbound: Outbound, session_id: &str) -> Self {
        Self {
            outbound,
            session_id: session_id.to_owned(),
        }
    }

    /// The bound session id.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Appends one typed event; the reply carries the committed envelope
    /// with its provider-assigned seq and time.
    ///
    /// # Errors
    ///
    /// Returns the transport failure or the reply contract mismatch.
    pub fn append(
        &self,
        type_name: &str,
        data: &serde_json::Value,
        surface_op: Option<&SurfaceOp>,
        source_event_seqs: Option<&[SessionSeq]>,
    ) -> Result<SessionEvent, SessionClientError> {
        let request = AppendRequest {
            id: &self.session_id,
            type_name,
            data,
            surface_op,
            source_event_seqs,
        };
        let body = serde_json::to_vec(&request)
            .map_err(|error| SessionClientError::Reply(error.to_string()))?;
        let reply = self
            .outbound
            .require(SERVICE_NAME, METHOD_APPEND, &body)
            .map_err(SessionClientError::Transport)?;
        let reply: AppendReply = serde_json::from_slice(&reply)
            .map_err(|error| SessionClientError::Reply(error.to_string()))?;
        Ok(reply.event)
    }
}

#[cfg(test)]
mod tests {
    use super::{METHOD_APPEND, SERVICE_NAME, SessionClient, SessionClientError};
    use crate::outbound::Outbound;
    use dougong_abi::{HostExports, STATUS_FAILED, STATUS_OK};
    use std::ffi::c_void;

    // Serves a reply outside the append contract for the decode-failure
    // test.
    unsafe extern "C" fn broken_require(
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
        // SAFETY: the vtable contract guarantees the caller-owned buffer.
        let written = unsafe { dougong_abi::write_out(b"{\"nope\":true}", out_buf, cap, out_len) };
        if written {
            STATUS_OK
        } else {
            dougong_abi::STATUS_CAPACITY
        }
    }

    /// A fake provider serving the documented append contract: seq starts
    /// at 1, time is fixed, and the committed envelope echoes the request.
    struct FakeSession {
        next_seq: u64,
        requests: Vec<(String, u32, serde_json::Value)>,
    }

    unsafe extern "C" fn serving_require(
        host: *mut c_void,
        service: *const u8,
        service_len: usize,
        method: u32,
        in_data: *const u8,
        in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the vtable contract passes valid readable slices for this
        // call; the handle aliases the test-owned service box.
        let fake = unsafe { &mut *(host.cast::<FakeSession>()) };
        // SAFETY: the vtable contract passes one valid readable slice per
        // argument for this call.
        let service = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(service, service_len)).into_owned()
        };
        // SAFETY: same per-call validity.
        let payload: serde_json::Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(in_data, in_len) })
                .expect("request is JSON");
        fake.requests
            .push((service.clone(), method, payload.clone()));
        if service != SERVICE_NAME || method != METHOD_APPEND {
            return STATUS_FAILED;
        }
        let seq = fake.next_seq;
        fake.next_seq += 1;
        let mut envelope = serde_json::json!({ "type": payload["type"], "seq": seq, "time": 1_700_000_000_000_i64, "data": payload["data"] });
        if let Some(op) = payload.get("surfaceOp") {
            envelope["surfaceOp"] = op.clone();
        }
        if let Some(seqs) = payload.get("sourceEventSeqs") {
            envelope["sourceEventSeqs"] = seqs.clone();
        }
        let reply = serde_json::json!({ "event": envelope });
        let bytes = serde_json::to_vec(&reply).expect("reply serializes");
        // SAFETY: the vtable contract guarantees the caller-owned buffer.
        let written = unsafe { dougong_abi::write_out(&bytes, out_buf, cap, out_len) };
        if written {
            STATUS_OK
        } else {
            dougong_abi::STATUS_CAPACITY
        }
    }

    unsafe extern "C" fn quiet_emit(
        _host: *mut c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    fn client() -> (SessionClient, *mut FakeSession) {
        let fake = Box::into_raw(Box::new(FakeSession {
            next_seq: 1,
            requests: Vec::new(),
        }));
        let outbound = Outbound::new(HostExports {
            host_handle: fake.cast::<c_void>(),
            require: serving_require,
            emit: quiet_emit,
        });
        (SessionClient::new(outbound, "s1"), fake)
    }

    fn release(fake: *mut FakeSession) -> Vec<(String, u32, serde_json::Value)> {
        // SAFETY: dispatch already returned synchronously; the box is
        // reclaimed exactly once.
        let fake = unsafe { Box::from_raw(fake) };
        fake.requests
    }

    #[test]
    fn append_encodes_the_contract_and_decodes_the_committed_event() {
        let (client, fake) = client();
        let data = serde_json::json!({ "target": "next-step", "start": 0, "inserted": [] });
        let event = client
            .append("agent/inbox/spliced", &data, None, None)
            .expect("append succeeds");
        let requests = release(fake);
        assert_eq!(requests.len(), 1);
        let (service, method, payload) = &requests[0];
        assert_eq!(service, "session-persistence");
        assert_eq!(*method, METHOD_APPEND);
        assert_eq!(
            payload,
            &serde_json::json!({
                "id": "s1",
                "type": "agent/inbox/spliced",
                "data": { "target": "next-step", "start": 0, "inserted": [] },
            })
        );
        assert_eq!(event.event_type(), "agent/inbox/spliced");
        assert_eq!(event.seq.get(), 1);
        assert_eq!(event.time, 1_700_000_000_000_i64);
    }

    #[test]
    fn append_passes_surface_metadata_through() {
        let (client, fake) = client();
        let data = serde_json::json!({
            "turn": 1,
            "step": 1,
            "message": {
                "id": "sys-1",
                "content": [{ "type": "text", "text": "prompt" }],
                "source": { "kind": "system-prompt" }
            }
        });
        let op = serde_json::from_value::<fr_wire::event::SurfaceOp>(serde_json::json!({
            "op": "replace", "startSeq": 2, "endSeq": 2
        }))
        .expect("fixture surface op");
        let event = client
            .append("system/message", &data, Some(&op), Some(&[event_seq(4)]))
            .expect("append succeeds");
        let requests = release(fake);
        let (_, _, payload) = &requests[0];
        assert_eq!(payload["surfaceOp"]["op"], "replace");
        assert_eq!(payload["sourceEventSeqs"], serde_json::json!([4]));
        assert!(matches!(
            event.surface_op,
            Some(fr_wire::event::SurfaceOp::Replace(_))
        ));
    }

    #[test]
    fn append_surfaces_transport_and_reply_failures() {
        // A host rejection surfaces as the transport arm.
        unsafe extern "C" fn failing_require(
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
            // SAFETY: the vtable contract guarantees the caller-owned buffer.
            let _ = unsafe { dougong_abi::write_out(b"{}", out_buf, cap, out_len) };
            STATUS_FAILED
        }
        let outbound = Outbound::new(HostExports {
            host_handle: std::ptr::null_mut(),
            require: failing_require,
            emit: quiet_emit,
        });
        let client = SessionClient::new(outbound, "s1");
        let error = client
            .append("turn/start", &serde_json::json!({ "turn": 1 }), None, None)
            .expect_err("the host rejects the call");
        assert!(matches!(error, super::SessionClientError::Transport(_)));

        // A provider reply outside the contract fails the decode instead.
        let outbound = Outbound::new(HostExports {
            host_handle: std::ptr::null_mut(),
            require: broken_require,
            emit: quiet_emit,
        });
        let client = SessionClient::new(outbound, "s1");
        let error = client
            .append("turn/start", &serde_json::json!({ "turn": 1 }), None, None)
            .expect_err("reply misses the event field");
        assert!(matches!(error, SessionClientError::Reply(_)));
        assert!(error.to_string().contains("reply is invalid"));
    }

    fn event_seq(value: u64) -> fr_wire::ids::SessionSeq {
        fr_wire::ids::SessionSeq::new(value).expect("fixture seq is a safe integer")
    }

    /// Removes the integration test's temporary backend root.
    struct TempRoot(std::path::PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Routes one require into the real fr-session service dispatch.
    unsafe extern "C" fn real_dispatch_require(
        host: *mut c_void,
        _service: *const u8,
        _service_len: usize,
        method: u32,
        in_data: *const u8,
        in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the handle aliases the test-owned service box.
        let service =
            unsafe { &*(host.cast::<std::sync::Mutex<fr_session::service::SessionService>>()) };
        // SAFETY: the payload slice is valid for this call per the vtable
        // contract.
        let payload = unsafe { std::slice::from_raw_parts(in_data, in_len) };
        let mut service = service.lock().expect("service lock");
        match fr_session::service::dispatch(method, payload, &mut service) {
            Ok(reply) => {
                // SAFETY: the vtable contract guarantees the caller-owned
                // buffer.
                let written = unsafe { dougong_abi::write_out(&reply, out_buf, cap, out_len) };
                if written {
                    STATUS_OK
                } else {
                    dougong_abi::STATUS_CAPACITY
                }
            }
            Err(text) => {
                let body = serde_json::json!({ "error": text }).to_string();
                // SAFETY: same caller-owned buffer contract.
                let _ = unsafe { dougong_abi::write_out(body.as_bytes(), out_buf, cap, out_len) };
                STATUS_FAILED
            }
        }
    }

    /// Pins the consumer wrapper against the real fr-session service
    /// dispatch: create over method 2, append over method 3, and the
    /// committed envelope round-trips the documented contract (the
    /// service-name wiring itself is Phase 18 assembly).
    #[test]
    fn append_round_trips_against_the_real_session_service() {
        let root =
            std::env::temp_dir().join(format!("fr-agent-loop-session-it-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("temp root");
        let _guard = TempRoot(root.clone());

        let mut vocabulary = fr_session::vocabulary::EventVocabulary::core();
        vocabulary
            .declare(&["agent/inbox/spliced".to_owned()])
            .expect("declare the loop's extension type");
        let service = fr_session::service::SessionService::new(
            &root,
            vocabulary,
            std::sync::Arc::new(|| 1_700_000_000_000_i64),
            std::sync::Arc::new(|_warning: String| {}),
        )
        .expect("assemble the real service");
        let service = std::sync::Mutex::new(service);
        let handle = Box::into_raw(Box::new(service));

        let outbound = Outbound::new(HostExports {
            host_handle: handle.cast::<c_void>(),
            require: real_dispatch_require,
            emit: quiet_emit,
        });
        let client = SessionClient::new(outbound, "s1");

        let create = serde_json::json!({ "id": "s1" });
        let reply = {
            // SAFETY: same handle aliasing as the require path.
            let service =
                unsafe { &*handle.cast::<std::sync::Mutex<fr_session::service::SessionService>>() };
            let mut service = service.lock().expect("service lock");
            fr_session::service::dispatch(
                fr_session::service::METHOD_SESSION_CREATE,
                create.to_string().as_bytes(),
                &mut service,
            )
            .expect("create the session")
        };
        assert_eq!(reply, b"{\"sessionId\":\"s1\"}".to_vec());

        let data = serde_json::json!({
            "target": "next-step",
            "start": 0,
            "inserted": []
        });
        let event = client
            .append("agent/inbox/spliced", &data, None, None)
            .expect("append over the real dispatch");
        assert_eq!(event.seq.get(), 0);
        assert_eq!(event.event_type(), "agent/inbox/spliced");

        // SAFETY: both dispatch paths returned synchronously; the box is
        // reclaimed exactly once.
        drop(unsafe { Box::from_raw(handle) });
    }
}
