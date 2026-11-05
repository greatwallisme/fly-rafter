// Rust guideline compliant 2026-11-05
//! The fr-llm-retry wire vocabulary, single-sourced in the owner crate.
//!
//! The driver consumes this crate's rlib for the decision chain value and
//! the retry-started event shape instead of carrying local copies (the
//! `tools_client` judgment).

use serde::{Deserialize, Serialize};

/// Durable, non-surface record of one provider-routed retry scheduled
/// after a failed request attempt (dsh `llm/retry`).
pub const EVENT_LLM_RETRY: &str = "llm/retry";
/// Durable transition written after a retry wait completes and before
/// the next request attempt starts (dsh `llm/retry-started`).
pub const EVENT_LLM_RETRY_STARTED: &str = "llm/retry-started";

/// The kernel-bus event this plugin listens on (the loop's request-error
/// waterfall; `fr_wire::services::events::EVENT_REQUEST_ERROR` mirrored
/// for the descriptor literal without linking the loop crate).
pub const LISTEN_EVENT: &str = "agent/request-error";

/// The dylib artifact name (the descriptor's `name`; the manifest row
/// resolves the artifact by the same stem).
pub const PLUGIN_NAME: &str = "fr-llm-retry";

/// The decision chain value's kind (the dsh backoff's post-wait
/// `{kind:'retry'}` twin).
pub const RETRY_KIND: &str = "retry";

/// Whether a scheduled retry is bounded (`normal`) or unbounded
/// (`always`; the dsh `ResolvedRetryPolicy` mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryMode {
    /// Bounded retries of the configured retryable codes.
    #[serde(rename = "normal")]
    Normal,
    /// Unbounded retries until success, cancellation, or disposal.
    #[serde(rename = "always")]
    Always,
}

/// Durable payload recorded before one provider-routed retry wait.
///
/// The dsh `LlmRetryEventData` 1:1: the two mode variants collapse to
/// one struct because `maxRetries` exists on `normal` only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmRetryEventData {
    /// Stable identity shared by every attempt in one retry chain.
    pub retry_id: String,
    /// The turn the failed attempt belonged to.
    pub turn: u64,
    /// The step the failed attempt belonged to.
    pub step: u64,
    /// The provider route that failed.
    pub provider: String,
    /// The policy mode the retry rides.
    pub mode: RetryMode,
    /// The policy identity key (the dsh `policyKey`).
    pub policy_key: String,
    /// The retry attempt this wait leads into (1-based within the step).
    pub retry: u64,
    /// Maximum retries (`normal` only; absent on `always`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u64>,
    /// The scheduled wait, in milliseconds (the dsh float kept exact).
    pub delay_ms: f64,
    /// The structured failure facts, carried verbatim.
    pub failure: fr_wire::Failure,
}

/// Durable transition recorded after one retry delay completes
/// (the dsh `LlmRetryStartedEventData` twin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmRetryStartedEventData {
    /// The retry chain the wait belonged to.
    pub retry_id: String,
    /// The turn the retry re-enters.
    pub turn: u64,
    /// The step the retry re-enters.
    pub step: u64,
    /// The retry attempt that starts.
    pub retry: u64,
}

/// The fr-fabric chain value a retry ruling replies with.
///
/// The dsh post-wait `{kind:'retry'}` plus the wait facts moved to the
/// loop's cancellable park (18c Q1A). Plain `{"kind":"retry"}` stays
/// decodable for the in-process registry compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryDecisionWire {
    /// The decision kind; only `"retry"` rules a retry.
    pub kind: String,
    /// The loop-side wait, in milliseconds (the dsh float kept exact).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<f64>,
    /// The retry chain identity (`llm/retry-started` needs it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_id: Option<String>,
    /// The retry attempt the wait leads into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<u64>,
}

/// Encodes the scheduled-retry decision: the exact wait for the loop's
/// park, the chain identity, and the retry number.
#[must_use]
pub fn retry_decision_json(retry: u64, retry_id: &str, delay_ms: f64) -> Vec<u8> {
    serde_json::to_vec(&RetryDecisionWire {
        kind: RETRY_KIND.to_owned(),
        delay_ms: Some(delay_ms),
        retry_id: Some(retry_id.to_owned()),
        retry: Some(retry),
    })
    .unwrap_or_default()
}

/// One decoded chain ruling.
///
/// The bare dsh form `{"kind":"retry"}` rules an immediate retry with no
/// durable identity (the loop skips the `llm/retry-started` append); the
/// full two-stage form carries the wait facts (18c Q1A).
#[derive(Debug, Clone, PartialEq)]
pub struct RetryDecision {
    /// The loop-side wait, in milliseconds (bare form: zero).
    pub delay_ms: f64,
    /// The retry chain identity, when the ruling carries it.
    pub retry_id: Option<String>,
    /// The retry attempt the wait leads into, when carried.
    pub retry: Option<u64>,
}

/// Decodes a chain reply.
///
/// `Some` only when the reply rules a retry; `None` keeps the chain
/// untouched (any other value falls back to the loop's in-process
/// default).
#[must_use]
pub fn decode_retry_decision(bytes: &[u8]) -> Option<RetryDecision> {
    let wire: RetryDecisionWire = serde_json::from_slice(bytes).ok()?;
    if wire.kind != RETRY_KIND {
        return None;
    }
    Some(RetryDecision {
        delay_ms: wire.delay_ms.unwrap_or(0.0),
        retry_id: wire.retry_id,
        retry: wire.retry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_round_trips_the_full_identity() {
        let bytes = retry_decision_json(2, "r-1", 1_500.0);
        let decoded = decode_retry_decision(&bytes).expect("a full ruling");
        assert_eq!(decoded.delay_ms, 1_500.0);
        assert_eq!(decoded.retry_id.as_deref(), Some("r-1"));
        assert_eq!(decoded.retry, Some(2));
    }

    #[test]
    fn bare_kind_stays_an_immediate_identity_free_retry() {
        let decoded = decode_retry_decision(br#"{"kind":"retry"}"#).expect("the bare form");
        assert_eq!(decoded.delay_ms, 0.0);
        assert_eq!(decoded.retry_id, None);
        assert_eq!(decoded.retry, None);
        assert_eq!(decode_retry_decision(br#"{"kind":"other"}"#), None);
        assert_eq!(decode_retry_decision(b"not json"), None);
    }

    #[test]
    fn retry_event_data_round_trips_camel_case() {
        let data = LlmRetryEventData {
            retry_id: "r-1".to_owned(),
            turn: 1,
            step: 2,
            provider: "deepseek-official".to_owned(),
            mode: RetryMode::Normal,
            policy_key: "k".to_owned(),
            retry: 1,
            max_retries: Some(5),
            delay_ms: 450.0,
            failure: fr_wire::Failure {
                message: "m".to_owned(),
                code: "SERVER".to_owned(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
                offload_images: None,
            },
        };
        let encoded = serde_json::to_string(&data).expect("encode");
        assert!(
            encoded.contains("\"retryId\":\"r-1\"") && encoded.contains("\"policyKey\":\"k\""),
            "camelCase wire: {encoded}"
        );
        let decoded: LlmRetryEventData = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, data);
        let started = LlmRetryStartedEventData {
            retry_id: "r-1".to_owned(),
            turn: 1,
            step: 2,
            retry: 2,
        };
        let encoded = serde_json::to_string(&started).expect("encode");
        assert!(encoded.contains("\"retryId\":\"r-1\""));
    }
}
