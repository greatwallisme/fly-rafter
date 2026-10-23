//! Test fixtures shared across session-domain module tests (compile in
//! test builds only).

#![cfg(test)]

use fr_wire::call_config::{CallConfig, CallControls, ToolSchema};
use fr_wire::content::{ContentBlock, KnownContentBlock, RawObject, TrueOnly};
use fr_wire::event::{
    AppendOp, DeveloperMessageData, RequestHeaderData, RequestHeaderReason, SessionEvent,
    SessionEventData, SurfaceOp,
};
use fr_wire::ids::{MessageId, SessionSeq};
use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};

/// Admits `n` as a session sequence, panicking on the impossible rejection.
#[must_use]
pub fn seq(n: u64) -> SessionSeq {
    SessionSeq::new(n).unwrap_or_else(|e| panic!("seq {n} rejected: {e}"))
}

/// A request/header event declaring one deferred tool.
#[must_use]
pub fn header_tool(n: u64, name: &str) -> SessionEvent {
    SessionEvent {
        seq: seq(n),
        time: 1,
        data: SessionEventData::RequestHeader(RequestHeaderData {
            header: fr_wire::event::EpochHeader {
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
            reason: RequestHeaderReason::Initial,
            starts_series: None,
        }),
        ignorable: None,
        surface_op: None,
        source_event_seqs: None,
    }
}

/// A developer/message event adding one tool through its header reference.
#[must_use]
pub fn developer_addition(n: u64, header_seq: u64, tool_name: &str) -> SessionEvent {
    SessionEvent {
        seq: seq(n),
        time: 1,
        data: SessionEventData::DeveloperMessage(DeveloperMessageData {
            turn: 0,
            step: 0,
            message: Message::Known(KnownMessage::Developer {
                id: MessageId::new(format!("d-{n}")),
                content: vec![ContentBlock::Known(KnownContentBlock::ToolAddition {
                    tool_name: tool_name.to_owned(),
                })],
                source: MessageSource::Known(KnownMessageSource::User {
                    extra: RawObject::new(),
                }),
            }),
            header_seq: Some(seq(header_seq)),
        }),
        ignorable: None,
        surface_op: Some(SurfaceOp::Append(AppendOp)),
        source_event_seqs: None,
    }
}
