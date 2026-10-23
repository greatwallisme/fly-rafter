//! Raw model streaming vocabulary and the compact durable stream records
//! (dsh `dsh-llm` types.ts `StreamChunk` + assistant-stream.ts
//! `AssistantStreamRecord` verbatim).

use crate::call_config::{FinishReason, ReplayEnvelope, TokenUsage};
use crate::content::ContentBlock;
use crate::ids::ToolCallId;
use serde::{Deserialize, Serialize};

/// Raw streaming protocol emitted by adapters; block indexes correlate
/// interleaved deltas and `block-end` carries the assembled block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KnownStreamChunk {
    /// Opens a content block at `index`.
    #[serde(rename = "block-start")]
    #[serde(rename_all = "camelCase")]
    BlockStart {
        /// Block position among interleaved blocks.
        index: u64,
        /// The block type tag; plugin-added blocks extend the vocabulary.
        block_type: String,
    },
    /// Appends visible text to the block at `index`.
    #[serde(rename = "text-delta")]
    #[serde(rename_all = "camelCase")]
    TextDelta {
        /// Block position.
        index: u64,
        /// The appended text.
        text: String,
    },
    /// Appends reasoning content to the block at `index`.
    #[serde(rename = "reasoning-delta")]
    #[serde(rename_all = "camelCase")]
    ReasoningDelta {
        /// Block position.
        index: u64,
        /// The appended reasoning text.
        text: String,
    },
    /// Appends tool-call arguments to the block at `index`.
    #[serde(rename = "tool-call-delta")]
    #[serde(rename_all = "camelCase")]
    ToolCallDelta {
        /// Block position.
        index: u64,
        /// Provider-issued call id.
        id: ToolCallId,
        /// Tool name, present on the first delta of a call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The appended raw JSON arguments fragment.
        arguments_delta: String,
    },
    /// Closes the block at `index` with the assembled block.
    #[serde(rename = "block-end")]
    #[serde(rename_all = "camelCase")]
    BlockEnd {
        /// Block position.
        index: u64,
        /// The assembled block.
        block: ContentBlock,
    },
    /// Reports token usage; emitted before the terminal finish and never after.
    #[serde(rename = "usage")]
    #[serde(rename_all = "camelCase")]
    Usage {
        /// The reported accounting.
        usage: TokenUsage,
    },
    /// Terminal chunk of one stream attempt.
    #[serde(rename = "finish")]
    #[serde(rename_all = "camelCase")]
    Finish {
        /// Why the response stopped.
        reason: FinishReason,
        /// Replay metadata for a successful response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay_state: Option<ReplayEnvelope>,
    },
}

/// One stream chunk: a known kind, or an adapter-added kind retained
/// verbatim (the chunk union is merge-extensible).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StreamChunk {
    /// A known chunk kind.
    Known(KnownStreamChunk),
    /// An unknown chunk kind, retained verbatim.
    Other(crate::content::RawObject),
}

/// One compact durable stream record from an assistant settlement or attempt.
///
/// Delta chunks pack into runs (`time0` start timestamp, `dt` inter-event
/// gaps, packed payloads); every never-packed chunk type stays a raw `chunk`
/// record holding the exact original chunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssistantStreamRecord {
    /// Packed visible-text delta run.
    #[serde(rename = "text-chunks")]
    #[serde(rename_all = "camelCase")]
    TextChunks {
        /// First chunk timestamp (epoch ms).
        time0: i64,
        /// Block position.
        index: u64,
        /// Inter-event gaps in milliseconds, one per packed text.
        dt: Vec<i64>,
        /// Packed visible texts.
        texts: Vec<String>,
    },
    /// Packed reasoning delta run.
    #[serde(rename = "reasoning-chunks")]
    #[serde(rename_all = "camelCase")]
    ReasoningChunks {
        /// First chunk timestamp (epoch ms).
        time0: i64,
        /// Block position.
        index: u64,
        /// Inter-event gaps in milliseconds, one per packed text.
        dt: Vec<i64>,
        /// Packed reasoning texts.
        texts: Vec<String>,
    },
    /// Packed tool-call delta run.
    #[serde(rename = "tool-call-chunks")]
    #[serde(rename_all = "camelCase")]
    ToolCallChunks {
        /// First chunk timestamp (epoch ms).
        time0: i64,
        /// Block position.
        index: u64,
        /// Inter-event gaps in milliseconds, one per packed fragment.
        dt: Vec<i64>,
        /// Provider-issued call id shared by the run.
        id: ToolCallId,
        /// Tool name; present only when the deltas carried one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Packed raw JSON argument fragments.
        args: Vec<String>,
    },
    /// A raw, never-packed chunk (block boundaries, usage, finish, and
    /// malformed tool-call deltas without id or name).
    #[serde(rename = "chunk")]
    #[serde(rename_all = "camelCase")]
    Chunk {
        /// Chunk timestamp (epoch ms).
        time: i64,
        /// The exact original chunk.
        chunk: StreamChunk,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_config::FinishReasonKind;

    #[test]
    fn stream_chunk_keys_are_verbatim_camel_case() {
        let chunk = StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
            index: 0,
            id: ToolCallId::new("c-9"),
            name: Some("bash".to_owned()),
            arguments_delta: "{\"".to_owned(),
        });
        let json = serde_json::to_value(&chunk).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(
            json.get("type").and_then(serde_json::Value::as_str),
            Some("tool-call-delta")
        );
        assert_eq!(
            json.get("argumentsDelta")
                .and_then(serde_json::Value::as_str),
            Some("{\"")
        );
    }

    #[test]
    fn stream_record_packed_run_round_trips() {
        let record = AssistantStreamRecord::ToolCallChunks {
            time0: 1_700_000_000_000,
            index: 2,
            dt: vec![0, 12, 3],
            id: ToolCallId::new("c-9"),
            name: Some("bash".to_owned()),
            args: vec!["{".to_owned(), "\"cmd\"".to_owned()],
        };
        let json = serde_json::to_value(&record).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(
            json.get("time0").and_then(serde_json::Value::as_i64),
            Some(1_700_000_000_000)
        );
        let back: AssistantStreamRecord =
            serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, record);
    }

    #[test]
    fn finish_chunk_carries_replay_envelope() {
        let chunk = StreamChunk::Known(KnownStreamChunk::Finish {
            reason: FinishReason::Known(FinishReasonKind::Stop),
            replay_state: Some(ReplayEnvelope {
                response: serde_json::json!({"id": "resp-1"}),
                blocks: Some(vec![serde_json::json!({"i": 0})]),
            }),
        });
        let json = serde_json::to_value(&chunk).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(
            json["replayState"]["response"]["id"],
            serde_json::json!("resp-1")
        );
    }
}
