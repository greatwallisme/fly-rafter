//! Call configuration, failure, usage, tool schema, and tool history wire
//! shapes (dsh `dsh-llm` types.ts / call-config.ts verbatim).

use crate::content::{RawObject, TrueOnly};
use crate::ids::{MessageId, ProviderRequestId, ReasoningEffortId};
use serde::{Deserialize, Serialize};

/// Serializable provider or transport failure facts; policy decides whether
/// they are retryable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    /// Human-readable provider or transport failure.
    pub message: String,
    /// Stable provider-neutral machine-routing code.
    pub code: String,
    /// HTTP status returned by the provider, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Provider-requested delay in milliseconds, when valid and available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_retry_after_ms: Option<u64>,
    /// Opaque provider-issued request identifier for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<ProviderRequestId>,
    /// With code `IMAGE_OFFLOAD_REQUIRED`: how many more of the oldest
    /// retained image occurrences the route needs offloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offload_images: Option<u64>,
}

/// Token accounting for one model call (cache fields optional). Counts are
/// disjoint: `inputTokens` is uncached input only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// Uncached input token count.
    pub input_tokens: u64,
    /// Output token count.
    pub output_tokens: u64,
    /// Exact full-call total when the adapter can supply one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Cached input tokens reported separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Cache-write tokens reported separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Reasoning token count when the provider reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

/// Why a model response stopped: a known reason, or an adapter-specific
/// reason retained verbatim (merge-extensible).
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "the Other fallback embeds serde_json::Map (f64-containing Value), so Eq is not derivable"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FinishReason {
    /// A known reason.
    Known(FinishReasonKind),
    /// An unknown reason, retained verbatim.
    Other(RawObject),
}

/// Known finish reasons, tagged by the wire key `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FinishReasonKind {
    /// Natural stop.
    #[serde(rename = "stop")]
    Stop,
    /// The model requested tool invocations.
    #[serde(rename = "tool-calls")]
    ToolCalls,
    /// The response hit the output-token ceiling.
    #[serde(rename = "max-tokens")]
    MaxTokens,
    /// Cancellation interrupted the stream.
    #[serde(rename = "aborted")]
    Aborted {
        /// The cancellation failure facts.
        failure: Failure,
    },
    /// The stream failed.
    #[serde(rename = "error")]
    Error {
        /// The failure facts.
        failure: Failure,
    },
}

/// Adapter-private lossless-JSON state for replaying a successful response,
/// carried by a terminal `finish` chunk.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayEnvelope {
    /// Response-level adapter-private metadata.
    pub response: serde_json::Value,
    /// Per-block adapter-private metadata, one entry per emitted block in
    /// first-seen stream order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<serde_json::Value>>,
}

/// JSON-schema description of a tool, as sent to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSchema {
    /// Requests deferred loading of the tool definition into model context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<TrueOnly>,
    /// Tool name.
    pub name: String,
    /// Tool description.
    pub description: String,
    /// JSON Schema object for the arguments.
    pub parameters: RawObject,
}

/// One logged developer-message tool update.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "ToolSchema embeds serde_json::Map (f64-containing Value), so Eq is not derivable"
)]
#[serde(rename_all = "camelCase")]
pub struct ToolHistoryUpdate {
    /// Identity used to locate this update in the derived request history.
    pub message_id: MessageId,
    /// Added definitions resolved from the event's referenced request header.
    pub additions: Vec<ToolSchema>,
}

/// Logged tool declarations and update identities since the last declaration
/// reset.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolHistory {
    /// Complete active declarations at the start of this history.
    pub tools: Vec<ToolSchema>,
    /// Ordered developer messages, additions resolved from historical headers.
    pub updates: Vec<ToolHistoryUpdate>,
}

/// Concrete generation settings; omitted controls use the route's defaults.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallControls {
    /// Adapter-owned reasoning effort selected for the exact model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Per-request output cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Stop sequences; generation halts when the model produces any one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// Provider, model, reasoning effort, and sampling scalars of one
/// conversation's requests; the loop builds requests from the logged header.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallConfig {
    /// Provider route key.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Sampling controls.
    #[serde(flatten)]
    pub controls: CallControls,
}

/// Effective config fields supplied by exact-model adapter resolution rather
/// than by the caller's request proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterDefaults {
    /// The route's default reasoning effort was materialized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<TrueOnly>,
    /// The route's default output cap was materialized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<TrueOnly>,
}

/// How a model applies a system prompt that changes mid-conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SystemPromptUpdate {
    /// The model reads the latest system message at any position.
    InHistory,
}

/// How a model accepts native tool declarations that change mid-conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolUpdate {
    /// Reads additions and removals in developer messages.
    #[serde(rename = "in-history")]
    InHistory,
    /// Reads additions only.
    #[serde(rename = "addition-only")]
    AdditionOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_config_flattens_controls_to_verbatim_keys() {
        let config = CallConfig {
            provider: "deepseek-official".to_owned(),
            model: "deepseek-flash".to_owned(),
            controls: CallControls {
                reasoning_effort: Some(ReasoningEffortId::new("low")),
                temperature: None,
                max_tokens: Some(4096),
                stop: Some(vec!["END".to_owned()]),
            },
        };
        let json = serde_json::to_value(&config).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(
            json.get("provider").and_then(serde_json::Value::as_str),
            Some("deepseek-official")
        );
        assert_eq!(
            json.get("reasoningEffort")
                .and_then(serde_json::Value::as_str),
            Some("low")
        );
        assert_eq!(
            json.get("maxTokens").and_then(serde_json::Value::as_u64),
            Some(4096)
        );
        let back: CallConfig =
            serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, config);
    }

    #[test]
    fn adapter_defaults_wire_literal_trues() {
        let defaults = AdapterDefaults {
            reasoning_effort: Some(TrueOnly),
            max_tokens: None,
        };
        let json = serde_json::to_value(defaults).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(
            json.get("reasoningEffort"),
            Some(&serde_json::Value::Bool(true))
        );
        assert!(json.get("maxTokens").is_none());
    }

    #[test]
    fn finish_reason_unknown_kind_is_retained() {
        let raw = serde_json::json!({"kind": "safety", "detail": "x"});
        let reason: FinishReason =
            serde_json::from_value(raw.clone()).unwrap_or_else(|_| panic!("retained"));
        assert!(matches!(reason, FinishReason::Other(_)));
        assert_eq!(
            serde_json::to_value(&reason).unwrap_or_else(|_| serde_json::Value::Null),
            raw
        );
    }
}
