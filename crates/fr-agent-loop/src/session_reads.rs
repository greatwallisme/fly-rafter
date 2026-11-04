//! The production [`SessionReads`] face over the `session-persistence`
//! service (the wire reads the driver's restore folds consume).
//!
//! The fold ownership stays with the session service: `restore` and
//! `system_prompt_nodes` re-read the log slice (reply events plus the
//! fr-additive ordered surface list) and fold through the same runtime
//! helpers the live path uses, `request_header`/`tool_history`/
//! `surface_generation`/`derive_messages` ride their dedicated service
//! arms, and `request_context` scans the freshly read log — every read
//! observes the current durable surface, so no loop-local fold can miss
//! another writer's mutation (driver.rs:103 contract).

use fr_wire::call_config::ToolHistory;
use fr_wire::event::RequestContextData;
use fr_wire::ids::SessionSeq;
use fr_wire::message::Message;
use fr_wire::services::session_persistence as wire;

use crate::driver::{RestoreView, SessionReads};
use crate::outbound::Outbound;
use crate::runtime_context::system_prompt_nodes;
use crate::session_client::SessionClientError;
use fr_wire::event::EpochHeader;
use fr_wire::event::SessionEvent;

/// Reads one session through the `session-persistence` service.
#[derive(Debug)]
pub struct SessionReadsClient {
    outbound: Outbound,
    session_id: String,
}

impl SessionReadsClient {
    /// Builds the read face over the activation's outbound seam.
    #[must_use]
    pub fn new(outbound: Outbound, session_id: &str) -> Self {
        Self {
            outbound,
            session_id: session_id.to_owned(),
        }
    }

    /// Performs one service read, mapping failures to the client error.
    fn read(
        &self,
        method: u32,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, SessionClientError> {
        let body = serde_json::to_vec(payload)
            .map_err(|error| SessionClientError::Reply(error.to_string()))?;
        let reply = self
            .outbound
            .require(wire::SERVICE_NAME, method, &body)
            .map_err(SessionClientError::Transport)?;
        serde_json::from_slice(&reply).map_err(|error| SessionClientError::Reply(error.to_string()))
    }

    /// One fresh log slice: the events plus the ordered surface seqs.
    fn read_log(&self) -> Result<(Vec<SessionEvent>, Vec<SessionSeq>), SessionClientError> {
        let reply = self.read(
            wire::METHOD_SESSION_READ,
            &serde_json::json!({ "id": self.session_id }),
        )?;
        let events: Vec<SessionEvent> = serde_json::from_value(
            reply
                .get("events")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|error| SessionClientError::Reply(format!("log events decode: {error}")))?;
        let seqs: Vec<u64> = serde_json::from_value(
            reply
                .get("surface")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|error| SessionClientError::Reply(format!("log surface decode: {error}")))?;
        let surface = seqs
            .into_iter()
            .map(SessionSeq::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| SessionClientError::Reply(format!("surface seq: {error}")))?;
        Ok((events, surface))
    }
}

impl SessionReads for SessionReadsClient {
    fn restore(&mut self) -> RestoreView {
        let (events, surface) = match self.read_log() {
            Ok(loaded) => loaded,
            Err(error) => fail_read("restore", &error),
        };
        RestoreView {
            events,
            surface: surface.into_iter().collect(),
        }
    }

    fn request_header(&mut self) -> Option<EpochHeader> {
        let reply = self
            .read(
                wire::METHOD_REQUEST_HEADER,
                &serde_json::json!({ "id": self.session_id }),
            )
            .unwrap_or_else(|error| fail_read("request-header", &error));
        serde_json::from_value(
            reply
                .get("header")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .unwrap_or_else(|error| {
            fail_read(
                "request-header decode",
                &SessionClientError::Reply(error.to_string()),
            )
        })
    }

    fn request_context(&mut self) -> Option<RequestContextData> {
        let (events, _) = self
            .read_log()
            .unwrap_or_else(|error| fail_read("request-context", &error));
        events.iter().rev().find_map(|event| match &event.data {
            fr_wire::event::SessionEventData::RequestContext(holder) => {
                Some(holder.context.clone())
            }
            _ => None,
        })
    }

    fn derive_messages(&mut self) -> Vec<Message> {
        let reply = self
            .read(
                wire::METHOD_DERIVE_MESSAGES,
                &serde_json::json!({ "id": self.session_id }),
            )
            .unwrap_or_else(|error| fail_read("derive-messages", &error));
        serde_json::from_value(
            reply
                .get("messages")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .unwrap_or_else(|error| {
            fail_read(
                "derive-messages decode",
                &SessionClientError::Reply(error.to_string()),
            )
        })
    }

    fn tool_history(&mut self) -> ToolHistory {
        let reply = self
            .read(
                wire::METHOD_TOOL_HISTORY,
                &serde_json::json!({ "id": self.session_id }),
            )
            .unwrap_or_else(|error| fail_read("tool-history", &error));
        serde_json::from_value(
            reply
                .get("history")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .unwrap_or_else(|error| {
            fail_read(
                "tool-history decode",
                &SessionClientError::Reply(error.to_string()),
            )
        })
    }

    fn surface_generation(&mut self) -> u64 {
        let reply = self
            .read(
                wire::METHOD_SURFACE_GENERATION,
                &serde_json::json!({ "id": self.session_id }),
            )
            .unwrap_or_else(|error| fail_read("surface-generation", &error));
        reply
            .get("generation")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| {
                fail_read(
                    "surface-generation decode",
                    &SessionClientError::Reply("missing generation".to_owned()),
                )
            })
    }

    fn system_prompt_nodes(&mut self) -> Vec<crate::runtime_context::SystemPromptNode> {
        let (events, surface) = self
            .read_log()
            .unwrap_or_else(|error| fail_read("system-prompt-nodes", &error));
        let by_seq: std::collections::HashMap<u64, &SessionEvent> = events
            .iter()
            .map(|event| (event.seq.get(), event))
            .collect();
        let surface_pairs = surface
            .iter()
            .filter_map(|seq| by_seq.get(&seq.get()).map(|event| (*seq, *event)));
        system_prompt_nodes(surface_pairs)
    }
}

/// Fails loudly on a read failure. The [`SessionReads`] trait is
/// infallible by design (the dsh face throws; the fr trait face has no
/// error channel), so the throw equivalent is the process-level failure:
/// a transport or decode failure here means a broken composition, and
/// carrying a stale fold forward would violate the driver.rs:103
/// authoritative-fold contract.
///
/// # Panics
///
/// Always: the failure names the read arm and the transport or decode
/// error.
fn fail_read(what: &str, error: &SessionClientError) -> ! {
    panic!("session read {what} failed: {error}");
}
