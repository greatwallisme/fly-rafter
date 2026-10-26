//! `DeepSeek`'s supported subset of the Anthropic Messages request protocol
//! (dsh `wire-types.ts`).
//!
//! Wire field spellings are protocol-exempt and stay verbatim; only
//! responses from the provider read these types back, so unknown
//! provider-side additions stay tolerated on deserialization.

use serde::{Deserialize, Serialize};

/// One request image source: inline base64 bytes or a Files API reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Inline base64 payload.
    Base64 {
        /// IANA media type of the encoded bytes.
        media_type: String,
        /// Base64-encoded image bytes.
        data: String,
    },
    /// A file id previously uploaded through the Files API.
    File {
        /// The provider-side file id.
        file_id: String,
    },
}

/// Text and inline or file-referenced images accepted in user messages and
/// tool results (dsh `WireInput`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireInput {
    /// Literal text content.
    Text {
        /// The text.
        text: String,
    },
    /// One image with its source.
    Image {
        /// Where the image bytes come from.
        source: ImageSource,
    },
}

/// A tool reference block body (dsh `{ type: 'tool_reference', name }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReference {
    /// Discriminator; always `tool_reference`.
    #[serde(rename = "type")]
    pub kind: ToolReferenceKind,
    /// The declared tool name being activated or removed.
    pub name: String,
}

/// The only tool reference discriminator value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolReferenceKind {
    /// `tool_reference`.
    ToolReference,
}

/// A reasoning block with its optional replay signature (dsh
/// `{ type: 'thinking', thinking, signature? }`); the `type` tag is
/// injected by the [`WireBlock`] enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingBlock {
    /// The reasoning text.
    pub thinking: String,
    /// Replay signature carried through from the source block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// A model tool call (dsh `{ type: 'tool_use', id, name, input }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUseBlock {
    /// Provider-issued call id.
    pub id: String,
    /// Called tool name.
    pub name: String,
    /// Parsed tool input object.
    pub input: serde_json::Value,
}

/// A tool result (dsh `{ type: 'tool_result', tool_use_id, content, is_error? }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultBlock {
    /// The call id this result answers.
    pub tool_use_id: String,
    /// Result content blocks.
    pub content: Vec<WireInput>,
    /// Failure marker; omitted on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Content serialized into one Messages conversation turn; capable routes
/// retain later system updates in message history (dsh `WireBlock`).
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "the ToolUse variant carries serde_json::Value, which has no Eq; \
              wire parity keeps the provider JSON value verbatim"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireBlock {
    /// Literal text content.
    Text {
        /// The text.
        text: String,
    },
    /// One image with its source.
    Image {
        /// Where the image bytes come from.
        source: ImageSource,
    },
    /// Activates a declared tool by name in conversation order.
    ToolAddition {
        /// The tool reference body.
        tool: ToolReference,
    },
    /// Removes a declared tool by name in conversation order.
    ToolRemoval {
        /// The tool reference body.
        tool: ToolReference,
    },
    /// A reasoning block, possibly with a replay signature.
    Thinking(ThinkingBlock),
    /// A model tool call.
    ToolUse(ToolUseBlock),
    /// A tool result.
    ToolResult(ToolResultBlock),
}

/// Conversation turn role (dsh `WireMessage.role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireRole {
    /// User turn.
    User,
    /// Assistant turn.
    Assistant,
    /// System turn (capable routes keep later updates in history).
    System,
}

/// One conversation turn (dsh `WireMessage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireMessage {
    /// Turn role.
    pub role: WireRole,
    /// Content blocks in conversation order.
    pub content: Vec<WireBlock>,
}

/// Reasoning toggle (dsh `thinking: { type: 'enabled' | 'disabled' }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// The `type` field.
    #[serde(rename = "type")]
    pub kind: ThinkingKind,
}

/// Reasoning toggle value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingKind {
    /// Reasoning enabled.
    Enabled,
    /// Reasoning disabled.
    Disabled,
}

/// Extended reasoning effort knob (dsh `output_config.effort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputEffort {
    /// Low effort.
    Low,
    /// High effort.
    High,
    /// Maximum effort.
    Max,
}

/// Extended reasoning effort config (dsh `output_config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputConfig {
    /// The effort knob.
    pub effort: OutputEffort,
}

/// One declared tool (dsh `WireRequest.tools[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTool {
    /// Tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
    /// JSON Schema for the tool input.
    pub input_schema: serde_json::Value,
    /// Delays tool availability until a `tool_addition` block activates it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
}

/// JSON body submitted to the resolved Messages endpoint (dsh `WireRequest`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRequest {
    /// Wire model id.
    pub model: String,
    /// Streaming requests only; always `true`.
    pub stream: bool,
    /// Per-request output-token cap.
    pub max_tokens: u64,
    /// Conversation turns.
    pub messages: Vec<WireMessage>,
    /// Effective system prompt; omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Reasoning toggle; always present.
    pub thinking: ThinkingConfig,
    /// Extended reasoning effort; omitted unless configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    /// Sampling temperature; omitted unless provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Stop sequences; omitted unless provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Tool declarations; omitted for tool-less requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<WireTool>>,
}

impl WireRequest {
    /// Starts a streaming request body for one model (dsh keeps
    /// `stream: true` on every request).
    pub fn streaming(model: impl Into<String>, max_tokens: u64) -> Self {
        Self {
            model: model.into(),
            stream: true,
            max_tokens,
            messages: Vec::new(),
            system: None,
            thinking: ThinkingConfig {
                kind: ThinkingKind::Enabled,
            },
            output_config: None,
            temperature: None,
            stop_sequences: None,
            tools: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wire_request_serializes_dsh_fields() {
        let request = WireRequest {
            system: Some("sys".into()),
            output_config: Some(OutputConfig {
                effort: OutputEffort::High,
            }),
            temperature: Some(0.5),
            stop_sequences: Some(vec!["END".into()]),
            tools: Some(vec![WireTool {
                name: "t".into(),
                description: "d".into(),
                input_schema: json!({"type": "object"}),
                defer_loading: Some(true),
            }]),
            ..WireRequest::streaming("deepseek-flash", 256_000)
        };
        let wire = serde_json::to_value(&request).expect("serialize");
        let object = wire.as_object().expect("object");
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "model",
                "stream",
                "max_tokens",
                "messages",
                "system",
                "thinking",
                "output_config",
                "temperature",
                "stop_sequences",
                "tools",
            ]
        );
        assert_eq!(wire["stream"], json!(true));
        assert_eq!(wire["thinking"], json!({"type": "enabled"}));
        assert_eq!(wire["output_config"], json!({"effort": "high"}));
        assert_eq!(wire["tools"][0]["input_schema"], json!({"type": "object"}));
        assert_eq!(wire["tools"][0]["defer_loading"], json!(true));
    }

    #[test]
    fn wire_blocks_roundtrip_all_forms() {
        let blocks = vec![
            WireBlock::Text { text: "hi".into() },
            WireBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".into(),
                    data: "ZD".into(),
                },
            },
            WireBlock::Image {
                source: ImageSource::File {
                    file_id: "file_1".into(),
                },
            },
            WireBlock::ToolAddition {
                tool: ToolReference {
                    kind: ToolReferenceKind::ToolReference,
                    name: "t".into(),
                },
            },
            WireBlock::ToolRemoval {
                tool: ToolReference {
                    kind: ToolReferenceKind::ToolReference,
                    name: "t".into(),
                },
            },
            WireBlock::Thinking(ThinkingBlock {
                thinking: "r".into(),
                signature: Some("s".into()),
            }),
            WireBlock::ToolUse(ToolUseBlock {
                id: "c1".into(),
                name: "t".into(),
                input: json!({"a": 1}),
            }),
            WireBlock::ToolResult(ToolResultBlock {
                tool_use_id: "c1".into(),
                content: vec![WireInput::Text { text: "out".into() }],
                is_error: None,
            }),
        ];
        for block in &blocks {
            let wire = serde_json::to_value(block).expect("serialize");
            assert_eq!(
                wire["type"],
                serde_json::to_value(block).expect("x")["type"]
            );
            let back: WireBlock = serde_json::from_value(wire).expect("deserialize");
            assert_eq!(&back, block);
        }
        let thinking = serde_json::to_value(&blocks[5]).expect("thinking");
        assert_eq!(
            thinking,
            json!({"type": "thinking", "thinking": "r", "signature": "s"})
        );
        let result = serde_json::to_value(&blocks[7]).expect("result");
        assert_eq!(
            result,
            json!({"type": "tool_result", "tool_use_id": "c1", "content": [{"type": "text", "text": "out"}]})
        );
        let file_image = serde_json::to_value(&blocks[2]).expect("file image");
        assert_eq!(
            file_image,
            json!({"type": "image", "source": {"type": "file", "file_id": "file_1"}})
        );
    }
}
