//! Compaction vocabulary (dsh compaction/src/types.ts + index.ts 1:1).
//!
//! The result type, the four log-only `compaction/*` shadow-event payloads,
//! the manual-compaction failure classification, and the trigger vocabulary.
//! The dsh `SessionEventMap` merges declare these as plugin events; fr keeps
//! them `Unknown` at the fr-wire layer (fr-session validates the pairing
//! structurally — the v4 `compact()` checks), and fr-compaction owns the
//! typed payload shapes.

use fr_wire::call_config::TokenUsage;
use fr_wire::{ContentBlock, SessionSeq, TrueOnly};
use serde::{Deserialize, Serialize};

use crate::brand::CompactionId;

/// Wire type names of the compaction shadow events (the dsh
/// `SessionEventMap` keys).
pub mod event_type {
    /// `compaction/start` — the log-only lock holder.
    pub const START: &str = "compaction/start";
    /// `compaction/summary` — the completed summary metering event.
    pub const SUMMARY: &str = "compaction/summary";
    /// `compaction/end` — the lock release.
    pub const END: &str = "compaction/end";
    /// `compaction/prune` — the prune replacement's shadow price.
    pub const PRUNE: &str = "compaction/prune";
}

/// Why automatic policy is asking a backend to consider compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    /// Normal step-boundary pressure.
    Pressure,
    /// Provider-confirmed context overflow.
    ContextOverflow,
}

impl CompactionTrigger {
    /// The dsh trigger string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pressure => "pressure",
            Self::ContextOverflow => "context-overflow",
        }
    }
}

/// Expected failure classes for an explicit idle-session compaction request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualCompactionErrorCode {
    /// The agent was active (or waking work was queued).
    Busy,
    /// The request's cancellation signal fired.
    Cancelled,
    /// The selected span changed under the transaction.
    Changed,
    /// Summarization or the shrink gate failed.
    Summary,
    /// The commit stage (replacement append or surface replacement) failed.
    Commit,
    /// The durability flush failed.
    Persistence,
}

impl ManualCompactionErrorCode {
    /// The dsh code string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
            Self::Changed => "changed",
            Self::Summary => "summary",
            Self::Commit => "commit",
            Self::Persistence => "persistence",
        }
    }
}

/// Expected manual-compaction failure suitable for a direct human-command
/// result. Shared durable-lock entry assertions may also throw the `busy`
/// subtype from automatic compaction paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualCompactionError {
    /// Stable failure class; `busy` may originate from any compaction path.
    pub code: ManualCompactionErrorCode,
    message: String,
}

impl ManualCompactionError {
    /// Create one classified compaction failure; the message is the backend
    /// diagnostic retained as the display text.
    pub fn new(code: ManualCompactionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The backend diagnostic.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ManualCompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ManualCompactionError {}

/// A surface-boundary pair naming a shadowed span.
///
/// The seqs of the first (`start`) and last (`end`) surface nodes of the
/// replaced range. A surface-POSITION span, not a numeric seq interval —
/// after a prior replace lands a fresh high-seq summary node at an older
/// range's position, `start` can be greater than `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowedRange {
    /// Inclusive first surface-node seq of the replaced range.
    pub start: SessionSeq,
    /// Inclusive last surface-node seq of the replaced range.
    pub end: SessionSeq,
}

/// The `compaction/start` data object: marks the start of a compaction —
/// log-only, holds the lock until `compaction/end`.
///
/// A numbered owner is strictly enclosed by that open turn; `None` (wire
/// `null`) identifies a standalone manual transaction between turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionStartData {
    /// Owning compaction identity.
    pub compaction_id: CompactionId,
    /// Initiating manual command, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_command_id: Option<String>,
    /// The owning turn ordinal, or wire `null` for a standalone transaction.
    pub turn: Option<u64>,
}

/// The `compaction/summary` data object: completed summary, its inputs, and
/// its model call facts — log-only, no surfaceOp.
///
/// The summary content is in `summary`; the actual surface replacement is
/// performed by the immediately following `user/message` event that shadows
/// the compacted range. That adjacency is contractual — the shadowed pricing
/// fields are the replacement's shadow price, so a consumer may pair a
/// replacement with the metering event directly before it
/// (`compaction/prune` documents the shared protocol).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryData {
    /// Owning compaction identity.
    pub compaction_id: CompactionId,
    /// Initiating manual command, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_command_id: Option<String>,
    /// The summary content blocks produced by the backend.
    pub summary: Vec<ContentBlock>,
    /// The surface-boundary pair that was shadowed.
    pub shadowed_range: ShadowedRange,
    /// The seqs of all shadowed surface nodes, in surface order.
    pub shadowed_seqs: Vec<SessionSeq>,
    /// Estimated token count of the shadowed content.
    pub shadowed_token_count: u64,
    /// The provider route that wrote the summary.
    pub provider: String,
    /// The model that wrote the summary — the summarize call's envelope,
    /// reported by the backend that made the call, logged so the one-shot
    /// request is reconstructable from log + code.
    pub model: String,
    /// The generation cap the summarize call sent, when one applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Provider-reported token usage for the summarization request, when
    /// emitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    /// Complete provider output before the backend's safe summary
    /// projection. dsh declares it required exactly on the `llmStreamCall`
    /// arm; fr keeps the optional face of the second arm (the union's wire
    /// faces are identical up to the optional key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<Vec<ContentBlock>>,
    /// Marks the summary the product of exactly one call through this
    /// context's LLM seam; absent for an unmarked template, remote, or other
    /// summarizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_stream_call: Option<TrueOnly>,
}

/// The `compaction/end` data object: marks the end of a compaction —
/// log-only, releases the lock.
///
/// Its owner matches `compaction/start`; `error` records an unsuccessful
/// attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEndData {
    /// Owning compaction identity.
    pub compaction_id: CompactionId,
    /// Initiating manual command, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_command_id: Option<String>,
    /// The owning turn ordinal, or wire `null` for a standalone transaction.
    pub turn: Option<u64>,
    /// The failure diagnostic, when the attempt was unsuccessful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The `compaction/prune` data object: shadow price of one model-free prune
/// replacement — log-only, no surfaceOp.
///
/// The shared shadow-price protocol: a surface `replace` event is priced by
/// the metering event immediately before it (`compaction/summary` for a
/// summarizing compaction, this event for a prune), which states the
/// heuristic token price of the exact replaced range so a pure consumer can
/// subtract it without retaining per-node prices. The replacement MUST be
/// appended synchronously right after this event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPruneData {
    /// The replaced range's first and last surface-node seqs (a
    /// surface-position span, like [`CompactionResult::shadowed_range`]).
    pub shadowed_range: ShadowedRange,
    /// The seqs of all shadowed surface nodes, in surface order.
    pub shadowed_seqs: Vec<SessionSeq>,
    /// Heuristic price of the shadowed content under the token-meter's
    /// fixed estimator.
    pub shadowed_token_count: u64,
}

/// Result of a successful compaction operation. An in-process value (never
/// serialized; the durable record is the event sequence itself).
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    /// Stable identity shared by this compaction's complete durable
    /// lifecycle.
    pub compaction_id: CompactionId,
    /// Human command that initiated this compaction, when it was manual.
    pub source_command_id: Option<String>,
    /// The seq of the appended `compaction/start` event.
    pub start_seq: SessionSeq,
    /// The seq of the appended `compaction/summary` event.
    pub summary_seq: SessionSeq,
    /// The seq of the appended `compaction/end` event.
    pub end_seq: SessionSeq,
    /// The summary content blocks produced by the backend.
    pub summary: Vec<ContentBlock>,
    /// The surface-boundary pair that was shadowed.
    pub shadowed_range: ShadowedRange,
    /// The seqs of all shadowed surface nodes, in surface order.
    pub shadowed_seqs: Vec<SessionSeq>,
    /// Estimated token count of the shadowed content.
    pub shadowed_token_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seq(value: u64) -> SessionSeq {
        SessionSeq::new(value).unwrap_or_else(|error| panic!("seq: {error}"))
    }

    fn sample_summary() -> CompactionSummaryData {
        CompactionSummaryData {
            compaction_id: CompactionId("t1".to_owned()),
            source_command_id: None,
            summary: vec![
                serde_json::from_value(json!({
                    "type": "text", "text": "s"
                }))
                .expect("block"),
            ],
            shadowed_range: ShadowedRange {
                start: seq(2),
                end: seq(5),
            },
            shadowed_seqs: vec![seq(2), seq(3), seq(4), seq(5)],
            shadowed_token_count: 40,
            provider: "p".to_owned(),
            model: "m".to_owned(),
            max_tokens: None,
            usage: None,
            raw_output: None,
            llm_stream_call: None,
        }
    }

    #[test]
    fn start_serializes_the_standalone_null_turn() {
        let data = CompactionStartData {
            compaction_id: CompactionId("t1".to_owned()),
            source_command_id: None,
            turn: None,
        };
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            json!({"compactionId": "t1", "turn": null})
        );
        let owned = CompactionStartData {
            compaction_id: CompactionId("t2".to_owned()),
            source_command_id: Some("cmd".to_owned()),
            turn: Some(3),
        };
        assert_eq!(
            serde_json::to_value(&owned).expect("serialize"),
            json!({"compactionId": "t2", "sourceCommandId": "cmd", "turn": 3})
        );
    }

    #[test]
    fn summary_round_trips_both_union_arms() {
        let unmarked = sample_summary();
        let raw = serde_json::to_value(&unmarked).expect("serialize");
        assert_eq!(
            raw,
            json!({
                "compactionId": "t1",
                "summary": [{"type": "text", "text": "s"}],
                "shadowedRange": {"start": 2, "end": 5},
                "shadowedSeqs": [2, 3, 4, 5],
                "shadowedTokenCount": 40,
                "provider": "p",
                "model": "m",
            })
        );
        let back: CompactionSummaryData = serde_json::from_value(raw).expect("deserialize");
        assert_eq!(back, unmarked);

        let marked = CompactionSummaryData {
            raw_output: Some(vec![]),
            llm_stream_call: Some(TrueOnly),
            ..sample_summary()
        };
        let marked_raw = serde_json::to_value(&marked).expect("serialize");
        assert_eq!(marked_raw["llmStreamCall"], json!(true));
        assert_eq!(marked_raw["rawOutput"], json!([]));
        let marked_back: CompactionSummaryData =
            serde_json::from_value(marked_raw).expect("deserialize");
        assert_eq!(marked_back, marked);
    }

    #[test]
    fn end_serializes_the_error_face() {
        let failed = CompactionEndData {
            compaction_id: CompactionId("t1".to_owned()),
            source_command_id: None,
            turn: None,
            error: Some("summary is not smaller".to_owned()),
        };
        let raw = serde_json::to_value(&failed).expect("serialize");
        assert_eq!(raw["error"], json!("summary is not smaller"));
        let clean = CompactionEndData {
            error: None,
            ..failed
        };
        let clean_raw = serde_json::to_value(&clean).expect("serialize");
        assert_eq!(clean_raw, json!({"compactionId": "t1", "turn": null}));
    }

    #[test]
    fn prune_carries_the_shared_shadow_price_protocol() {
        let data = CompactionPruneData {
            shadowed_range: ShadowedRange {
                start: seq(1),
                end: seq(3),
            },
            shadowed_seqs: vec![seq(1), seq(2), seq(3)],
            shadowed_token_count: 120,
        };
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            json!({
                "shadowedRange": {"start": 1, "end": 3},
                "shadowedSeqs": [1, 2, 3],
                "shadowedTokenCount": 120,
            })
        );
    }

    #[test]
    fn trigger_and_error_code_strings() {
        assert_eq!(CompactionTrigger::Pressure.as_str(), "pressure");
        assert_eq!(
            CompactionTrigger::ContextOverflow.as_str(),
            "context-overflow"
        );
        assert_eq!(ManualCompactionErrorCode::Busy.as_str(), "busy");
        assert_eq!(
            ManualCompactionErrorCode::Persistence.as_str(),
            "persistence"
        );
        let error = ManualCompactionError::new(
            ManualCompactionErrorCode::Busy,
            "manual compaction requires an idle agent with no waking queued work",
        );
        assert_eq!(error.code, ManualCompactionErrorCode::Busy);
        assert_eq!(
            error.to_string(),
            "manual compaction requires an idle agent with no waking queued work"
        );
    }
}
