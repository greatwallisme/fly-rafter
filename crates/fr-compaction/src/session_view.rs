//! The wire-backed session view (the dsh live `Session` face the
//! compaction transaction consumes, read over the `'session-persistence'`
//! service).
//!
//! One full read per open (compaction is rare and the transaction
//! re-reads nothing mid-flight); every [`SessionView::append`] reflects
//! in the local log, surface fold, and generation so subsequent reads
//! inside the same transaction see the appended events (the trait's
//! shell contract). The surface fold reuses the fr-session fold so the
//! node order and replacement generation match every other consumer.

use fr_session::{SurfaceFoldResult, fold_surface};
use fr_wire::event::EpochHeader;

use crate::plugin_shell::HostLink;
use crate::region::{AppendRequest, SessionView};

/// The owned view state: the durable log, its surface fold, and the
/// latest request-header snapshot.
pub struct RemoteSessionState {
    /// The session this view names.
    session_id: String,
    /// The durable log, in ascending seq order.
    events: Vec<fr_wire::SessionEvent>,
    /// The log's surface fold (nodes + generations).
    surface: SurfaceFoldResult,
    /// The latest durable `request/header` snapshot (the dsh
    /// `session.requestHeader()`; the `request-header` method face).
    header: Option<EpochHeader>,
}

impl std::fmt::Debug for RemoteSessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSessionState")
            .field("session_id", &self.session_id)
            .field("events", &self.events.len())
            .finish_non_exhaustive()
    }
}

impl RemoteSessionState {
    /// The current surface node seqs, in surface order.
    #[must_use]
    pub fn surface_nodes(&self) -> &[fr_wire::SessionSeq] {
        &self.surface.nodes
    }
}

/// The wire-backed session view; the transaction consumes it as
/// `&mut dyn SessionView`.
///
/// Interior mutability would fight the slice returns, so the state stays
/// plain and the shell rebuilds the view per handling.
pub struct RemoteSessionView {
    state: RemoteSessionState,
    link: HostLink,
}

impl std::fmt::Debug for RemoteSessionView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSessionView")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl RemoteSessionView {
    /// Opens one session: a full log read plus the latest request-header
    /// snapshot, folded.
    ///
    /// # Errors
    ///
    /// Returns the wire diagnostic verbatim (a not-live session is the
    /// caller's bug — the loop dispatches live sessions only).
    pub fn open(link: &HostLink, session_id: &str) -> Result<Self, String> {
        let read = link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            &serde_json::to_vec(&serde_json::json!({ "id": session_id }))
                .map_err(|error| error.to_string())?,
        )?;
        let reply: serde_json::Value = serde_json::from_slice(&read)
            .map_err(|error| format!("session read decode: {error}"))?;
        let raw_events = reply
            .get("events")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "session read reply requires an events array".to_owned())?;
        let mut events = Vec::with_capacity(raw_events.len());
        for raw in raw_events {
            let event: fr_wire::SessionEvent = serde_json::from_value(raw.clone())
                .map_err(|error| format!("session event decode: {error}"))?;
            events.push(event);
        }
        let header = link
            .require(
                fr_wire::services::session_persistence::SERVICE_NAME,
                fr_wire::services::session_persistence::METHOD_REQUEST_HEADER,
                &serde_json::to_vec(&serde_json::json!({ "id": session_id }))
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| format!("request header read failed: {error}"))?;
        let header_reply: serde_json::Value = serde_json::from_slice(&header)
            .map_err(|error| format!("request header decode: {error}"))?;
        let header: Option<EpochHeader> = match header_reply.get("header") {
            Some(value) if value.is_null() => None,
            Some(value) => Some(
                serde_json::from_value(value.clone())
                    .map_err(|error| format!("request header decode: {error}"))?,
            ),
            None => None,
        };
        let surface =
            fold_surface(&events).map_err(|error| format!("surface fold failed: {error}"))?;
        Ok(Self {
            state: RemoteSessionState {
                session_id: session_id.to_owned(),
                events,
                surface,
                header,
            },
            link: *link,
        })
    }

    /// Refolds the surface after a mutation.
    fn refold(&mut self) {
        self.state.surface = fold_surface(&self.state.events).unwrap_or_else(|error| {
            panic!("compaction session surface fold failed after append: {error}")
        });
    }
}

impl SessionView for RemoteSessionView {
    fn events(&self) -> &[fr_wire::SessionEvent] {
        &self.state.events
    }

    fn surface_nodes(&self) -> &[fr_wire::SessionSeq] {
        &self.state.surface.nodes
    }

    fn replace_generation(&self) -> u64 {
        self.state.surface.replace_generation
    }

    fn request_header(&self) -> Option<&EpochHeader> {
        self.state.header.as_ref()
    }

    fn append(&mut self, request: AppendRequest<'_>) -> Result<fr_wire::SessionEvent, String> {
        let mut body = serde_json::json!({
            "id": self.state.session_id,
            "type": request.type_name,
            "data": request.data,
        });
        if let Some(op) = request.surface_op {
            body["surfaceOp"] = serde_json::to_value(op)
                .map_err(|error| format!("surface op encode failed: {error}"))?;
        }
        if let Some(seqs) = request.source_event_seqs {
            body["sourceEventSeqs"] = serde_json::to_value(seqs)
                .map_err(|error| format!("source seqs encode failed: {error}"))?;
        }
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            &serde_json::to_vec(&body).map_err(|error| error.to_string())?,
        )?;
        let reply: serde_json::Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("append reply decode: {error}"))?;
        let stored = reply
            .get("event")
            .cloned()
            .ok_or_else(|| "append reply requires the stored event".to_owned())?;
        let event: fr_wire::SessionEvent = serde_json::from_value(stored)
            .map_err(|error| format!("stored event decode: {error}"))?;
        self.state.events.push(event.clone());
        self.refold();
        Ok(event)
    }
}
