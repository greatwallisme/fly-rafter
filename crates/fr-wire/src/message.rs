//! Conversation messages and producer sources, verbatim to dsh `dsh-llm`
//! message.ts wire shapes.
//!
//! Sources are merge-extensible: unknown producer kinds round-trip through
//! the `MessageSource::Other` fallback, and known kinds retain
//! producer-added fields via `#[serde(flatten)]`.

use crate::content::{ContentBlock, RawObject, TrueOnly};
use crate::ids::{MessageId, ToolCallId};
use serde::{Deserialize, Serialize};

/// One named contribution to a `snapshot`-form context, in assembly order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshotSection {
    /// The contributing subsystem's name.
    pub name: String,
    /// That contribution's model-facing text, exactly as assembled.
    pub text: String,
}

/// A known message source, tagged by the wire key `kind`. Producer-added
/// fields ride along through `extra` so known kinds stay verbatim too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum KnownMessageSource {
    /// A direct human prompt or a producer-supplied context message.
    #[serde(rename = "user")]
    #[serde(rename_all = "camelCase")]
    User {
        /// Producer-added source fields, retained verbatim.
        #[serde(flatten)]
        extra: RawObject,
    },
    /// A model-produced assistant source with adapter-private replay state.
    #[serde(rename = "model")]
    #[serde(rename_all = "camelCase")]
    Model {
        /// Provider route that produced the message.
        provider: String,
        /// Provider model id that produced the message.
        model: String,
        /// Lossless-JSON adapter state needed to replay the response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay_state: Option<serde_json::Value>,
        /// Producer-added source fields, retained verbatim.
        #[serde(flatten)]
        extra: RawObject,
    },
    /// A tool-role source carrying the answered call id.
    #[serde(rename = "tool")]
    #[serde(rename_all = "camelCase")]
    Tool {
        /// Provider-issued id of the tool call this source answers.
        call_id: ToolCallId,
        /// Producer-added source fields, retained verbatim.
        #[serde(flatten)]
        extra: RawObject,
    },
    /// A rendered system prompt attributed to the system-prompt producer.
    #[serde(rename = "system-prompt")]
    #[serde(rename_all = "camelCase")]
    SystemPrompt {
        /// Producer-added source extra fields, retained verbatim.
        #[serde(flatten)]
        extra: RawObject,
    },
}

/// One message source: a known kind, or an unknown producer kind retained
/// verbatim (dsh merge-extensible; consumers fall through unknown kinds).
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "the Other fallback embeds serde_json::Map (f64-containing Value), so Eq is not derivable"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageSource {
    /// A known producer kind.
    Known(KnownMessageSource),
    /// An unknown producer kind, retained verbatim.
    Other(RawObject),
}

/// Source aliases used by the role-specific message envelopes.
pub type Source = MessageSource;
/// The model source envelope of an assistant message.
pub type ModelSource = KnownMessageSource;
/// A system-prompt source; admission enforces the kind.
pub type SystemPromptSource = MessageSource;
/// A user-source envelope; admission enforces the kind.
pub type UserSource = MessageSource;
/// A tool-source envelope; admission enforces the kind.
pub type ToolSource = MessageSource;

/// A conversation message, tagged by the wire key `role`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum KnownMessage {
    /// A rendered system prompt; empty content sends no prompt.
    #[serde(rename = "system")]
    #[serde(rename_all = "camelCase")]
    System {
        /// Stable identity preserved across every representation boundary.
        id: MessageId,
        /// Exact model-facing blocks; empty content records "no system prompt".
        content: Vec<ContentBlock>,
        /// Producer source; always the system-prompt kind for this role.
        source: MessageSource,
    },
    /// A user-role message.
    #[serde(rename = "user")]
    #[serde(rename_all = "camelCase")]
    User {
        /// Stable identity.
        id: MessageId,
        /// Exact model-facing blocks.
        content: Vec<ContentBlock>,
        /// Producer source supplied by the producer.
        source: MessageSource,
    },
    /// A model-produced assistant message.
    #[serde(rename = "assistant")]
    #[serde(rename_all = "camelCase")]
    Assistant {
        /// Stable identity.
        id: MessageId,
        /// Exact model-facing blocks.
        content: Vec<ContentBlock>,
        /// Producer source; always the model kind for this role.
        source: MessageSource,
    },
    /// Incremental agent session changes in conversation order.
    #[serde(rename = "developer")]
    #[serde(rename_all = "camelCase")]
    Developer {
        /// Stable identity.
        id: MessageId,
        /// Exact model-facing blocks (tool additions and removals today).
        content: Vec<ContentBlock>,
        /// Producer source supplied by the producer.
        source: MessageSource,
    },
    /// A first-class tool-role message carrying one tool result.
    #[serde(rename = "tool")]
    #[serde(rename_all = "camelCase")]
    Tool {
        /// Stable identity.
        id: MessageId,
        /// Exact model-facing blocks.
        content: Vec<ContentBlock>,
        /// Producer source; always the tool kind for this role.
        source: MessageSource,
        /// Provider-issued id of the tool call this message answers.
        tool_call_id: ToolCallId,
        /// Whether the tool invocation failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<TrueOnly>,
    },
}

/// A durable conversation message: known role, or unknown role retained
/// verbatim (dsh message map is closed over persisted roles; the fallback
/// exists so a newer writer's log is never silently reshaped).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    /// A known role.
    Known(KnownMessage),
    /// An unknown role or shape, retained verbatim.
    Other(RawObject),
}

impl KnownMessage {
    /// The wire `role` tag of this message.
    #[must_use]
    pub const fn role_name(&self) -> &'static str {
        match self {
            Self::System { .. } => "system",
            Self::User { .. } => "user",
            Self::Assistant { .. } => "assistant",
            Self::Developer { .. } => "developer",
            Self::Tool { .. } => "tool",
        }
    }

    /// The message's stable identity.
    #[must_use]
    pub const fn id(&self) -> &MessageId {
        match self {
            Self::System { id, .. }
            | Self::User { id, .. }
            | Self::Assistant { id, .. }
            | Self::Developer { id, .. }
            | Self::Tool { id, .. } => id,
        }
    }

    /// The exact model-facing blocks.
    #[must_use]
    pub fn content(&self) -> &[ContentBlock] {
        match self {
            Self::System { content, .. }
            | Self::User { content, .. }
            | Self::Assistant { content, .. }
            | Self::Developer { content, .. }
            | Self::Tool { content, .. } => content,
        }
    }

    /// The producer source.
    #[must_use]
    pub const fn source(&self) -> &MessageSource {
        match self {
            Self::System { source, .. }
            | Self::User { source, .. }
            | Self::Assistant { source, .. }
            | Self::Developer { source, .. }
            | Self::Tool { source, .. } => source,
        }
    }

    /// The tool-call identity of a tool-role message, if this is one.
    #[must_use]
    pub const fn tool_call_id(&self) -> Option<&ToolCallId> {
        match self {
            Self::Tool { tool_call_id, .. } => Some(tool_call_id),
            _ => None,
        }
    }

    /// Whether the tool invocation failed; `None` off the tool role.
    #[must_use]
    pub const fn is_error(&self) -> Option<bool> {
        match self {
            Self::Tool { is_error, .. } => Some(is_error.is_some()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::KnownContentBlock;

    fn user_message() -> KnownMessage {
        KnownMessage::User {
            id: MessageId::new("m-1"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "hello".to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
        }
    }

    #[test]
    fn user_message_round_trips_with_verbatim_role_and_source() {
        let json = serde_json::to_value(user_message()).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(
            json.get("role").and_then(serde_json::Value::as_str),
            Some("user")
        );
        let source = json
            .get("source")
            .unwrap_or_else(|| panic!("source present"));
        assert_eq!(
            source.get("kind").and_then(serde_json::Value::as_str),
            Some("user")
        );
        let back: Message = serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, Message::Known(user_message()));
    }

    #[test]
    fn tool_message_wires_tool_call_id_camel_case() {
        let message = KnownMessage::Tool {
            id: MessageId::new("m-2"),
            content: vec![],
            source: MessageSource::Known(KnownMessageSource::Tool {
                call_id: ToolCallId::new("c-1"),
                extra: RawObject::new(),
            }),
            tool_call_id: ToolCallId::new("c-1"),
            is_error: Some(TrueOnly),
        };
        let json = serde_json::to_value(&message).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(
            json.get("toolCallId").and_then(serde_json::Value::as_str),
            Some("c-1")
        );
        assert_eq!(json.get("isError"), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn unknown_role_is_retained_verbatim() {
        let raw = serde_json::json!({
            "role": "plugin/ledger",
            "id": "m-3",
            "content": [],
            "source": {"kind": "plugin/recorder"}
        });
        let message: Message =
            serde_json::from_value(raw.clone()).unwrap_or_else(|_| panic!("retained"));
        match &message {
            Message::Other(obj) => assert_eq!(
                obj.get("role").and_then(serde_json::Value::as_str),
                Some("plugin/ledger")
            ),
            Message::Known(_) => panic!("unknown role must not parse as known"),
        }
        assert_eq!(
            serde_json::to_value(&message).unwrap_or_else(|_| serde_json::Value::Null),
            raw
        );
    }
}
