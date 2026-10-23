//! Content blocks and durable attachment references (dsh `dsh-llm` types.ts,
//! `dsh-attachment` types.ts verbatim wire shapes).

use crate::ids::AttachmentId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A JSON object with insertion-order preservation (workspace `serde_json`
/// `preserve_order` feature). Used to retain unknown wire variants verbatim.
pub type RawObject = serde_json::Map<String, serde_json::Value>;

/// A field that may only ever be `true` on the wire (dsh `offloaded?: true`,
/// `interrupted?: true`, `deferLoading?: true`, `startsSeries?: true`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TrueOnly;

impl Serialize for TrueOnly {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for TrueOnly {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = bool::deserialize(deserializer)?;
        if value {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("expected literal true"))
        }
    }
}

/// Raster image formats accepted by the version-one attachment path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageMediaType {
    /// PNG.
    #[serde(rename = "image/png")]
    Png,
    /// JPEG.
    #[serde(rename = "image/jpeg")]
    Jpeg,
    /// WebP.
    #[serde(rename = "image/webp")]
    Webp,
    /// GIF.
    #[serde(rename = "image/gif")]
    Gif,
}

/// Durable, serializable reference to one immutable normalized image
/// (`ImageAttachmentRef` verbatim).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachmentRef {
    /// Opaque storage identifier; never a filesystem path or bearer URL.
    pub attachment_id: AttachmentId,
    /// Media type verified from the stored bytes.
    pub media_type: ImageMediaType,
    /// Exact encoded byte length.
    pub bytes: u64,
    /// Intrinsic encoded width in pixels.
    pub width: u32,
    /// Intrinsic encoded height in pixels.
    pub height: u32,
    /// Optional display name stripped of local path information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Input dimensions after EXIF orientation, present only when
    /// normalization reduced the image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_dimensions: Option<OriginalDimensions>,
}

/// Input dimensions before normalization scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginalDimensions {
    /// Width before scaling.
    pub width: u32,
    /// Height before scaling.
    pub height: u32,
}

/// Durable, serializable reference to one verbatim stored file
/// (`FileAttachmentRef` verbatim); `attachment_id` is the sha256 digest of
/// exactly those bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachmentRef {
    /// Opaque content-addressed storage identifier.
    pub attachment_id: AttachmentId,
    /// Sanitized display filename, also the stored object's leaf name.
    pub name: String,
    /// Exact byte length.
    pub bytes: u64,
}

// NOTE: ImageRef/ReasoningRef do not exist on the dsh side and are not defined
// here; the lib re-export list is trimmed to the real shapes in this crate.

/// The known content-block variants, tagged by the wire key `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KnownContentBlock {
    /// Plain text visible to the end user.
    #[serde(rename = "text")]
    #[serde(rename_all = "camelCase")]
    Text {
        /// The text.
        text: String,
    },
    /// Reasoning / thinking content, distinct from visible text.
    #[serde(rename = "reasoning")]
    #[serde(rename_all = "camelCase")]
    Reasoning {
        /// The reasoning text.
        text: String,
    },
    /// A durable raster image reference, valid in user or assistant content.
    #[serde(rename = "image")]
    #[serde(rename_all = "camelCase")]
    Image {
        /// Immutable bytes and intrinsic display metadata owned by the
        /// attachment service.
        attachment: ImageAttachmentRef,
        /// Derived from a durable image-offload decision; every route sends
        /// substitute text instead of the bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offloaded: Option<TrueOnly>,
    },
    /// A durable verbatim file reference, valid in user content; request
    /// assembly projects files to deterministic handle text.
    #[serde(rename = "file")]
    #[serde(rename_all = "camelCase")]
    File {
        /// Immutable verbatim bytes and display metadata.
        attachment: FileAttachmentRef,
    },
    /// A tool invocation requested by the model.
    #[serde(rename = "tool-call")]
    #[serde(rename_all = "camelCase")]
    ToolCall {
        /// Provider-issued call id; correlates with the matching tool result.
        id: crate::ids::ToolCallId,
        /// Tool name.
        name: String,
        /// Raw JSON string as produced by the model.
        arguments: String,
    },
    /// Activates a tool definition from the developer event's referenced
    /// request header. The historical header owns the schema; an inline
    /// `tool` key is reserved and rejected by session admission.
    #[serde(rename = "tool-addition")]
    #[serde(rename_all = "camelCase")]
    ToolAddition {
        /// Name of exactly one tool in the referenced historical header.
        tool_name: String,
    },
    /// Records the dynamic removal of a tool identified by its session-local name.
    #[serde(rename = "tool-removal")]
    #[serde(rename_all = "camelCase")]
    ToolRemoval {
        /// The removed tool's session-local name.
        tool_name: String,
    },
}

/// One content block: a known variant, or an unknown plugin-added block
/// retained verbatim (the dsh content map is merge-extensible).
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "the Other fallback embeds serde_json::Map (f64-containing Value), so Eq is not derivable"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentBlock {
    /// A known core block.
    Known(KnownContentBlock),
    /// An unknown block shape, retained verbatim.
    Other(RawObject),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn true_only_serializes_as_literal_true_and_rejects_false() {
        let json = serde_json::to_string(&TrueOnly).unwrap_or_default();
        assert_eq!(json, "true");
        assert!(matches!(
            serde_json::from_str::<TrueOnly>("true"),
            Ok(TrueOnly)
        ));
        let _err = serde_json::from_str::<TrueOnly>("false").unwrap_err();
    }

    #[test]
    fn text_block_round_trips_with_verbatim_type_key() {
        let block = ContentBlock::Known(KnownContentBlock::Text {
            text: "hi".to_owned(),
        });
        let json = serde_json::to_value(&block).unwrap_or_else(|_| serde_json::Value::Null);
        let obj = json
            .as_object()
            .unwrap_or_else(|| panic!("expected object"));
        assert_eq!(
            obj.get("type").map(serde_json::Value::as_str),
            Some(Some("text"))
        );
        let back: ContentBlock =
            serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, block);
    }

    #[test]
    fn tool_addition_uses_camel_case_tool_name() {
        let block = KnownContentBlock::ToolAddition {
            tool_name: "bash".to_owned(),
        };
        let json = serde_json::to_value(&block).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(
            json.get("toolName").and_then(serde_json::Value::as_str),
            Some("bash")
        );
        assert!(json.get("tool_name").is_none());
    }

    #[test]
    fn unknown_block_type_is_retained_verbatim() {
        let raw = serde_json::json!({"type": "plugin/x-block", "custom": 1});
        let block: ContentBlock =
            serde_json::from_value(raw.clone()).unwrap_or_else(|_| panic!("unknown retained"));
        match &block {
            ContentBlock::Other(obj) => assert_eq!(obj.get("custom"), Some(&serde_json::json!(1))),
            ContentBlock::Known(_) => panic!("unknown must not parse as known"),
        }
        let out = serde_json::to_value(&block).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(out, raw);
    }
}
