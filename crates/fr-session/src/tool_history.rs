//! Capability-independent tool history projection (dsh tool-history.ts):
//! folds committed headers and developer messages into the baseline plus
//! resolved updates that request assembly consumes.

#![forbid(unsafe_code)]
use std::collections::HashMap;

use fr_wire::call_config::{ToolHistory, ToolSchema};
use fr_wire::content::{ContentBlock, KnownContentBlock};
use fr_wire::event::SessionEvent;
use fr_wire::message::KnownMessage;

/// Stateful reconstruction of historical tool definitions for request
/// projection.
#[derive(Debug, Clone, Default)]
pub struct ToolHistoryProjection {
    headers: HashMap<u64, Vec<ToolSchema>>,
    declared: Vec<(String, ToolSchema)>,
    baseline_seq: Option<u64>,
    history: ToolHistory,
    /// Active definitions from the latest request header (dsh `active`):
    /// the snapshot fallback returns this list, not the series baseline.
    active: Vec<ToolSchema>,
    available: Vec<String>,
}

impl ToolHistoryProjection {
    /// Consumes the next committed event in log order.
    ///
    /// # Panics
    ///
    /// Panics when a developer message's tool-addition names a tool its
    /// `headerSeq` header does not define — the dsh message is verbatim
    /// (`tool history: missing definition for {name}`), and the invariant is
    /// otherwise enforced at surface admission.
    pub fn apply(&mut self, event: &SessionEvent) {
        match &event.data {
            fr_wire::event::SessionEventData::RequestHeader(data) => {
                let tools = data.header.tools.clone().unwrap_or_default();
                self.headers.insert(event.seq.get(), tools.clone());
                let redeclared = tools.iter().any(|tool| {
                    self.declared
                        .iter()
                        .any(|(name, before)| name == &tool.name && wire(before) != wire(tool))
                });
                let new_series = self.baseline_seq.is_none()
                    || data.reason == fr_wire::event::RequestHeaderReason::Series
                    || data.starts_series.is_some()
                    || redeclared;
                if new_series {
                    self.baseline_seq = Some(event.seq.get());
                    self.declared = tools.iter().map(|t| (t.name.clone(), t.clone())).collect();
                    self.history = ToolHistory {
                        tools: tools.clone(),
                        updates: Vec::new(),
                    };
                    self.available = tools.iter().map(|t| t.name.clone()).collect();
                }
                self.active = tools;
            }
            fr_wire::event::SessionEventData::DeveloperMessage(data) => {
                let definitions = data
                    .header_seq
                    .as_ref()
                    .and_then(|seq| self.headers.get(&seq.get()));
                let mut additions: Vec<ToolSchema> = Vec::new();
                if let Some(message) = as_known(&data.message) {
                    for block in message.content() {
                        let ContentBlock::Known(KnownContentBlock::ToolAddition { tool_name }) =
                            block
                        else {
                            continue;
                        };
                        let Some(tool) = definitions.and_then(|defs| {
                            defs.iter().find(|tool| &tool.name == tool_name).cloned()
                        }) else {
                            panic!("tool history: missing definition for {tool_name}");
                        };
                        additions.push(tool);
                    }
                }
                for tool in &additions {
                    if let Some(slot) = self
                        .declared
                        .iter_mut()
                        .find(|(name, _)| name == &tool.name)
                    {
                        slot.1 = tool.clone();
                    } else {
                        self.declared.push((tool.name.clone(), tool.clone()));
                    }
                }
                for block in as_known(&data.message)
                    .map(KnownMessage::content)
                    .unwrap_or_default()
                {
                    match block {
                        ContentBlock::Known(KnownContentBlock::ToolAddition { tool_name }) => {
                            if !self.available.iter().any(|name| name == tool_name) {
                                self.available.push(tool_name.clone());
                            }
                        }
                        ContentBlock::Known(KnownContentBlock::ToolRemoval { tool_name }) => {
                            self.available.retain(|name| name != tool_name);
                        }
                        _ => {}
                    }
                }
                let message_id = as_known(&data.message).map_or_else(
                    || fr_wire::ids::MessageId::new(String::new()),
                    |message| message.id().clone(),
                );
                self.history
                    .updates
                    .push(fr_wire::call_config::ToolHistoryUpdate {
                        message_id,
                        additions,
                    });
            }
            _ => {}
        }
    }
}

impl ToolHistoryProjection {
    /// Reads an immutable snapshot; subsequent events do not mutate it.
    ///
    /// Sessions written before tool-update emission have headers without
    /// matching updates; those fall back to the plain active declarations.
    #[must_use]
    pub fn snapshot(&self) -> ToolHistory {
        let complete = self.active.len() == self.available.len()
            && self
                .active
                .iter()
                .all(|tool| self.available.iter().any(|name| name == &tool.name));
        if complete {
            self.history.clone()
        } else {
            ToolHistory {
                tools: self.active.clone(),
                updates: Vec::new(),
            }
        }
    }
}

/// Canonical wire form of a tool schema, for same-name definition drift
/// detection (dsh compares `JSON.stringify`).
fn wire(tool: &ToolSchema) -> String {
    serde_json::to_string(tool).unwrap_or_default()
}

const fn as_known(message: &fr_wire::message::Message) -> Option<&KnownMessage> {
    match message {
        fr_wire::message::Message::Known(known) => Some(&**known),
        fr_wire::message::Message::Other(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::{developer_addition, seq};
    use fr_wire::call_config::{CallConfig, CallControls};
    use fr_wire::content::{RawObject, TrueOnly};
    use fr_wire::event::{EpochHeader, RequestHeaderData, RequestHeaderReason, SessionEventData};
    use fr_wire::ids::MessageId;

    fn envelope(seq_value: u64, data: SessionEventData) -> SessionEvent {
        SessionEvent {
            seq: seq(seq_value),
            time: 1,
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn header(seq_value: u64, name: &str, series: bool) -> SessionEvent {
        envelope(
            seq_value,
            SessionEventData::RequestHeader(RequestHeaderData {
                header: EpochHeader {
                    config: CallConfig {
                        provider: "p".to_owned(),
                        model: "m".to_owned(),
                        controls: CallControls::default(),
                    },
                    adapter_defaults: None,
                    tools: Some(vec![ToolSchema {
                        defer_loading: Some(TrueOnly),
                        name: name.to_owned(),
                        description: "d".to_owned(),
                        parameters: RawObject::new(),
                    }]),
                },
                reason: if series {
                    RequestHeaderReason::Series
                } else {
                    RequestHeaderReason::Initial
                },
                starts_series: None,
            }),
        )
    }

    #[test]
    fn additions_resolve_through_header_seq_into_updates() {
        let mut projection = ToolHistoryProjection::default();
        projection.apply(&header(0, "bash", false));
        projection.apply(&developer_addition(1, 0, "bash"));
        let snapshot = projection.snapshot();
        assert_eq!(snapshot.updates.len(), 1);
        assert_eq!(snapshot.updates[0].additions.len(), 1);
        assert_eq!(snapshot.updates[0].additions[0].name, "bash");
        assert_eq!(snapshot.updates[0].message_id, MessageId::new("d-1"));
    }

    #[test]
    fn series_reset_drops_prefix_reuse() {
        let mut projection = ToolHistoryProjection::default();
        projection.apply(&header(0, "bash", false));
        projection.apply(&developer_addition(1, 0, "bash"));
        projection.apply(&header(2, "web", true));
        let snapshot = projection.snapshot();
        assert_eq!(snapshot.tools.len(), 1);
        assert_eq!(snapshot.tools[0].name, "web");
        assert_eq!(snapshot.updates, Vec::new());
    }
}
