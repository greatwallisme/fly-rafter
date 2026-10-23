//! Append- and restore-side admission checks over typed events (dsh
//! `validateSessionEventData`, `assertMessageEventShape`, and the seed
//! contiguity contract).

use crate::error::AdmissionError;
use fr_wire::content::ContentBlock;
use fr_wire::content::KnownContentBlock;
use fr_wire::event::SessionEvent;
use fr_wire::event::SessionEventData;
use fr_wire::event::event_type;
use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};

/// The surface-eligible role each message-producing event type must carry.
fn expected_role(event_type: &str) -> Option<&'static str> {
    match event_type {
        event_type::SYSTEM_MESSAGE => Some("system"),
        event_type::DEVELOPER_MESSAGE => Some("developer"),
        event_type::USER_MESSAGE => Some("user"),
        event_type::ASSISTANT_MESSAGE => Some("assistant"),
        event_type::TOOL_RESULT => Some("tool"),
        _ => None,
    }
}

/// Validates one event's payload invariants (dsh `validateSessionEventData`
/// plus the message-shape checks the seed boundary enforces).
///
/// # Errors
///
/// Returns the owning check's message when role pairing, tool-change
/// block shape, header sequencing, tool-error consistency, or message
/// identity invariants are violated.
pub fn validate_event_data(event: &SessionEvent) -> Result<(), AdmissionError> {
    match &event.data {
        SessionEventData::DeveloperMessage(data) => {
            validate_developer(data.turn, data.header_seq, &data.message)?;
            validate_message_shape(event.event_type(), &data.message)
        }
        SessionEventData::SystemMessage(data) => {
            validate_message_shape(event.event_type(), &data.message)
        }
        SessionEventData::UserMessage(message) => {
            validate_message_shape(event.event_type(), message)
        }
        SessionEventData::AssistantMessage(data) => {
            validate_message_shape(event.event_type(), &data.message)
        }
        SessionEventData::ToolResult(data) => {
            validate_message_shape(event.event_type(), &data.message)?;
            validate_tool_result(data, &data.message)
        }
        _ => Ok(()),
    }
}

fn validate_developer(
    turn: u64,
    header_seq: Option<fr_wire::ids::SessionSeq>,
    message: &Message,
) -> Result<(), AdmissionError> {
    let blocks = developer_blocks(message)?;
    let mut has_additions = false;
    for block in blocks {
        if let ContentBlock::Known(KnownContentBlock::ToolAddition { tool_name }) = block {
            has_additions = true;
            if tool_name.is_empty() {
                return Err(AdmissionError::new(
                    "session event tool-addition requires a nonempty toolName",
                ));
            }
        }
    }
    // The header reference is required exactly when additions are present.
    if has_additions != header_seq.is_some() {
        return Err(AdmissionError::new(
            "session event requires headerSeq exactly when tool additions are present",
        ));
    }
    let _ = turn;
    Ok(())
}

fn developer_blocks(message: &Message) -> Result<&[ContentBlock], AdmissionError> {
    match message {
        Message::Known(KnownMessage::Developer { content, .. }) => Ok(content),
        _ => Err(AdmissionError::new(
            "session event developer/message and developer role must occur together",
        )),
    }
}

fn validate_message_shape(event_type: &str, message: &Message) -> Result<(), AdmissionError> {
    let known = match message {
        Message::Known(known) => known,
        Message::Other(_) => {
            return Err(AdmissionError::new(format!(
                "session event {event_type} carries an unknown message role"
            )));
        }
    };
    let role = known.role_name();
    let expected = expected_role(event_type).unwrap_or_default();
    if role != expected {
        return Err(AdmissionError::new(format!(
            "session event {event_type} message must have role \"{expected}\""
        )));
    }
    if known.id().as_str().is_empty() {
        return Err(AdmissionError::new(format!(
            "session event {event_type} lacks an identified message"
        )));
    }
    match known {
        KnownMessage::System { source, .. } => {
            if source_kind(source) != Some("system-prompt") {
                return Err(AdmissionError::new(
                    "session event message must have system-prompt source",
                ));
            }
        }
        KnownMessage::Assistant { source, .. } => match source_kind(source) {
            Some("model") => {
                let (provider, model) = model_pair(source).ok_or_else(|| {
                    AdmissionError::new("session event message must have model source")
                })?;
                if provider.is_empty() || model.is_empty() {
                    return Err(AdmissionError::new(
                        "session event message must have model source",
                    ));
                }
            }
            _ => {
                return Err(AdmissionError::new(
                    "session event message must have model source",
                ));
            }
        },
        KnownMessage::Tool {
            source,
            tool_call_id,
            ..
        } => match source_kind(source) {
            Some("tool") => {
                let call = tool_call_id_of(source).ok_or_else(|| {
                    AdmissionError::new("session event message must have tool source")
                })?;
                if call.as_str().is_empty() || &call != tool_call_id {
                    return Err(AdmissionError::new(
                        "session event message has mismatched tool call ids",
                    ));
                }
            }
            _ => {
                return Err(AdmissionError::new(
                    "session event message must have tool source",
                ));
            }
        },
        KnownMessage::User { .. } | KnownMessage::Developer { .. } => {}
    }
    Ok(())
}

const fn source_kind(source: &MessageSource) -> Option<&str> {
    match source {
        MessageSource::Known(known) => match known {
            KnownMessageSource::User { .. } => Some("user"),
            KnownMessageSource::Model { .. } => Some("model"),
            KnownMessageSource::Tool { .. } => Some("tool"),
            KnownMessageSource::SystemPrompt { .. } => Some("system-prompt"),
        },
        MessageSource::Other(_) => None,
    }
}

const fn model_pair(source: &MessageSource) -> Option<(&str, &str)> {
    match source {
        MessageSource::Known(KnownMessageSource::Model {
            provider, model, ..
        }) => Some((provider.as_str(), model.as_str())),
        _ => None,
    }
}

fn tool_call_id_of(source: &MessageSource) -> Option<fr_wire::ids::ToolCallId> {
    match source {
        MessageSource::Known(KnownMessageSource::Tool { call_id, .. }) => Some(call_id.clone()),
        _ => None,
    }
}

const fn known_tool_is_error(message: &Message) -> Option<bool> {
    match message {
        Message::Known(KnownMessage::Tool { is_error, .. }) => Some(is_error.is_some()),
        _ => None,
    }
}

fn validate_tool_result(
    data: &fr_wire::event::ToolResultData,
    message: &Message,
) -> Result<(), AdmissionError> {
    if data.error.is_some() && known_tool_is_error(message) != Some(true) {
        return Err(AdmissionError::new(
            "session event error requires message.isError === true",
        ));
    }
    Ok(())
}
