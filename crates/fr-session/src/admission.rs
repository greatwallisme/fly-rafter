//! Append- and restore-side admission checks over typed events (dsh
//! `validateSessionEventData`, `assertMessageEventShape`, and the seed
//! contiguity contract).

#![forbid(unsafe_code)]
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
            validate_developer(data.header_seq, &data.message)?;
            validate_coordinates("developer/message", data.turn, data.step)?;
            validate_message_shape(event.event_type(), &data.message)
        }
        SessionEventData::SystemMessage(data) => {
            validate_coordinates("system/message", data.turn, data.step)?;
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
    Ok(())
}

fn developer_blocks(message: &Message) -> Result<&[ContentBlock], AdmissionError> {
    match message {
        Message::Known(known) if known.role_name() == "developer" => Ok(known.content()),
        _ => Err(AdmissionError::new(
            "session event developer/message and developer role must occur together",
        )),
    }
}

/// Rejects zero turn/step coordinates on the step-message families (the
/// write side of the stored positivity gates; PRD 19.2 item 17 — dsh live
/// appends lack this check, fr refuses at admission time).
fn validate_coordinates(event_type: &str, turn: u64, step: u64) -> Result<(), AdmissionError> {
    if turn == 0 {
        return Err(AdmissionError::new(format!(
            "session event {event_type} turn must be positive"
        )));
    }
    if step == 0 {
        return Err(AdmissionError::new(format!(
            "session event {event_type} step must be positive"
        )));
    }
    Ok(())
}

fn validate_message_shape(event_type: &str, message: &Message) -> Result<(), AdmissionError> {
    let known = match message {
        Message::Known(known) => known.as_ref(),
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

fn known_tool_is_error(message: &Message) -> Option<bool> {
    match message {
        Message::Known(known) if known.role_name() == "tool" => known.is_error(),
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

/// Whether one event type name follows the plugin-record grammar.
///
/// The grammar: `plugin:` followed by slash-separated segments of lowercase
/// letters, digits, `.`, `_`, and `-`, each starting with a letter or digit
/// (dsh types.ts:525).
#[must_use]
pub fn is_plugin_record_type(type_name: &str) -> bool {
    let Some(rest) = type_name.strip_prefix("plugin:") else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    rest.split('/').all(|segment| {
        let mut chars = segment.chars();
        match chars.next() {
            Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
            _ => return false,
        }
        chars.all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
        })
    })
}
