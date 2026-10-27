//! Terminal-failure classification and settlement (dsh llm-pi-ai
//! `stream.ts` `classifyPiAiError` + `mapStopReason`, Q76-B adapted
//! wording with verbatim code values).
//!
//! The classify chain pattern-matches error text because pi-ai flattens
//! the caught error to its message before the harness sees it (the dsh
//! XXX comment: transport detail rides `cause`, which upstream discards).
//! The fr face inherits the same input shape: every execution failure
//! arrives as one message string, so the same text classification runs.

use crate::codes::{INVALID_REQUEST_CODE, PI_AI_ERROR_CODE};
use crate::overflow::{OverflowStopKind, is_context_overflow};
use fr_llm::error::{
    AUTH_CODE, CONTEXT_WINDOW_EXCEEDED_CODE, EMPTY_RESPONSE_CODE, QUOTA_EXCEEDED_CODE,
    RATE_LIMIT_CODE, SERVER_CODE, TIMEOUT_CODE, TRANSPORT_CODE, is_context_window_exceeded_error,
    is_quota_exceeded_error,
};
use fr_wire::call_config::{Failure, FinishReason, FinishReasonKind};
use std::sync::OnceLock;

/// The classify arms in dsh order: (pattern source, `(?i)` where the dsh
/// regex carries `/i`, code). Compiled once, matched in order.
const CLASSIFY_ARMS: &[(&str, bool, &str)] = &[
    (r"\b(?:401|403)\b", false, AUTH_CODE),
    // The quota arm sits between 401/403 and 429 in the dsh chain and is
    // function-based there; it is inlined by the caller below.
    (r"\b429\b|rate.?limit", true, RATE_LIMIT_CODE),
    (
        r"\b413\b|failed to buffer the request body:\s*length limit exceeded|payload too large|request body too large",
        true,
        INVALID_REQUEST_CODE,
    ),
    (r"\b400\b|invalid.?request", true, INVALID_REQUEST_CODE),
    (r"\b5\d\d\b", false, SERVER_CODE),
    (r"\btime(?:d)?\s*out\b|timeout", true, TIMEOUT_CODE),
    (r"stream ended (?:before|without)\b", true, TRANSPORT_CODE),
    (
        r"\b(?:network|connection|socket|fetch)\b|\bECONN[A-Z]+\b",
        true,
        TRANSPORT_CODE,
    ),
    (
        r"\b(?:other side closed|HTTP2 request did not get a response|WebSocket closed unexpectedly)\b",
        true,
        TRANSPORT_CODE,
    ),
    (r"\bterminated\b|premature close", true, TRANSPORT_CODE),
];

fn arm_regex(index: usize) -> &'static regex::Regex {
    static ARMS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let arms = ARMS.get_or_init(|| {
        CLASSIFY_ARMS
            .iter()
            .map(|(source, icase, _)| {
                let pattern = if *icase {
                    format!("(?i){source}")
                } else {
                    (*source).to_owned()
                };
                regex::Regex::new(&pattern).expect("classify arm regex")
            })
            .collect()
    });
    &arms[index]
}

/// Classify one error message (dsh `classifyPiAiError`, arms in order;
/// the quota check keeps its function form between 401/403 and 429).
#[must_use]
pub fn classify_pi_ai_error(message: &str) -> &'static str {
    if arm_regex(0).is_match(message) {
        return AUTH_CODE;
    }
    if is_quota_exceeded_error(message) {
        return QUOTA_EXCEEDED_CODE;
    }
    // Arms 1.. (429 through premature close) run in order; the first hit
    // wins.
    for (index, (_, _, code)) in CLASSIFY_ARMS.iter().enumerate().skip(1) {
        if arm_regex(index).is_match(message) {
            return code;
        }
    }
    PI_AI_ERROR_CODE
}

/// The settlement snapshot one failed or aborted stream carries (the dsh
/// error event's `AssistantMessage` facts, fr-adapted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// The pi-ai stop reason the stream ended in (`"stop"`, `"length"`,
    /// `"toolUse"`, `"pending"`, `"aborted"`, `"error"`).
    pub stop: &'static str,
    /// The provider error message, when one exists.
    pub error_message: Option<String>,
    /// The route model id (request identity, for the message texts).
    pub model: String,
    /// The provider route key.
    pub provider: String,
    /// How many content blocks the stream produced before settling.
    pub blocks_len: usize,
    /// Usage input plus cache-read (the overflow comparison value).
    pub input_tokens: u64,
    /// Usage output tokens.
    pub output_tokens: u64,
    /// The route's context window, when declared (silent-overflow
    /// detection needs it).
    pub context_window: Option<u64>,
}

/// Settle one failed stream into the terminal finish reason (dsh
/// `mapStopReason` over the error-event arm; Q76-B: the "pi-ai" brand
/// reads "fr-llm-openai", code values verbatim).
#[must_use]
pub fn settle_failure(settlement: &Settlement) -> FinishReason {
    let failure = |message: String, code: &'static str| {
        FinishReason::Known(FinishReasonKind::Error {
            failure: Failure {
                message,
                code: code.to_owned(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
                offload_images: None,
            },
        })
    };
    if settlement.stop == "aborted" {
        let message = settlement
            .error_message
            .clone()
            .unwrap_or_else(|| "fr-llm-openai stream aborted".to_owned());
        return FinishReason::Known(FinishReasonKind::Aborted {
            failure: Failure {
                message,
                code: "ABORTED".to_owned(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
                offload_images: None,
            },
        });
    }
    // Overflow, both sources: the pi-ai pattern family over the message
    // and usage, and the harness context-window error text.
    let overflow = is_context_overflow(
        OverflowStopKind::Error,
        settlement.error_message.as_deref(),
        &settlement.provider,
        settlement.input_tokens,
        settlement.output_tokens,
        settlement.context_window,
    ) || settlement
        .error_message
        .as_deref()
        .is_some_and(is_context_window_exceeded_error);
    if overflow {
        let message = settlement.error_message.clone().unwrap_or_else(|| {
            format!(
                "fr-llm-openai detected context overflow for model \"{}\"",
                settlement.model
            )
        });
        return failure(message, CONTEXT_WINDOW_EXCEEDED_CODE);
    }
    match settlement.stop {
        // A terminal stop with no content blocks is a degenerate provider
        // completion, not a successful empty assistant message.
        "stop" if settlement.blocks_len == 0 => failure(
            format!(
                "model \"{}\" returned a completed response with no content",
                settlement.model
            ),
            EMPTY_RESPONSE_CODE,
        ),
        "stop" => FinishReason::Known(FinishReasonKind::Stop),
        "length" => FinishReason::Known(FinishReasonKind::MaxTokens),
        "toolUse" => FinishReason::Known(FinishReasonKind::ToolCalls),
        "pending" => failure(
            format!(
                "fr-llm-openai stream for model \"{}\" ended pending",
                settlement.model
            ),
            PI_AI_ERROR_CODE,
        ),
        _ => {
            let message = settlement
                .error_message
                .clone()
                .unwrap_or_else(|| "fr-llm-openai stream error".to_owned());
            let code = classify_pi_ai_error(&message);
            failure(message, code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settlement(stop: &'static str, message: Option<&str>) -> Settlement {
        Settlement {
            stop,
            error_message: message.map(str::to_owned),
            model: "gpt-test".to_owned(),
            provider: "openai".to_owned(),
            blocks_len: 1,
            input_tokens: 0,
            output_tokens: 0,
            context_window: None,
        }
    }

    fn code_of(reason: &FinishReason) -> String {
        match reason {
            FinishReason::Known(
                FinishReasonKind::Error { failure } | FinishReasonKind::Aborted { failure },
            ) => failure.code.clone(),
            _ => String::new(),
        }
    }

    /// Every classify arm in order, plus the fallback: the dsh example
    /// texts (401/403, quota, 429, the 413 body-size family, 400, 5xx,
    /// timeout wordings, the truncation wording, the transport words,
    /// undici's bare `terminated`).
    #[test]
    fn classify_pins_the_dsh_arm_order() {
        assert_eq!(classify_pi_ai_error("401 Unauthorized"), "AUTH");
        assert_eq!(
            classify_pi_ai_error("You exceeded your quota"),
            QUOTA_EXCEEDED_CODE
        );
        assert_eq!(classify_pi_ai_error("429 Too Many Requests"), "RATE_LIMIT");
        assert_eq!(
            classify_pi_ai_error("Rate limited: slow down"),
            "RATE_LIMIT"
        );
        assert_eq!(
            classify_pi_ai_error("failed to buffer the request body: length limit exceeded"),
            "INVALID_REQUEST"
        );
        assert_eq!(classify_pi_ai_error("400 Bad Request"), "INVALID_REQUEST");
        assert_eq!(
            classify_pi_ai_error("Invalid request payload"),
            "INVALID_REQUEST"
        );
        assert_eq!(classify_pi_ai_error("503 Service Unavailable"), "SERVER");
        assert_eq!(classify_pi_ai_error("Request timed out."), "TIMEOUT");
        assert_eq!(
            classify_pi_ai_error("OpenAI Responses stream ended before a terminal response event"),
            "TRANSPORT"
        );
        assert_eq!(classify_pi_ai_error("fetch failed"), "TRANSPORT");
        assert_eq!(classify_pi_ai_error("ECONNRESET"), "TRANSPORT");
        assert_eq!(classify_pi_ai_error("terminated"), "TRANSPORT");
        assert_eq!(classify_pi_ai_error("premature close"), "TRANSPORT");
        assert_eq!(classify_pi_ai_error("something novel"), PI_AI_ERROR_CODE);
    }

    /// The settlement arms: aborted, both overflow sources, the empty
    /// stop, the clean mappings, pending, and the classify fallback.
    #[test]
    fn settle_pins_every_arm() {
        let aborted = settle_failure(&settlement("aborted", None));
        assert!(matches!(
            aborted,
            FinishReason::Known(FinishReasonKind::Aborted { .. })
        ));

        let overflow_text = settle_failure(&settlement("error", Some("prompt is too long: 2 > 1")));
        assert_eq!(code_of(&overflow_text), CONTEXT_WINDOW_EXCEEDED_CODE);

        let mut harness_overflow = settlement("error", Some("context window exceeded"));
        harness_overflow.context_window = Some(1_000);
        harness_overflow.input_tokens = 2_000;
        assert_eq!(
            code_of(&settle_failure(&harness_overflow)),
            CONTEXT_WINDOW_EXCEEDED_CODE
        );

        let mut empty = settlement("stop", None);
        empty.blocks_len = 0;
        assert_eq!(code_of(&settle_failure(&empty)), EMPTY_RESPONSE_CODE);

        // The helper's default snapshot carries one block, so a plain
        // stop settles clean.
        assert!(matches!(
            settle_failure(&settlement("stop", None)),
            FinishReason::Known(FinishReasonKind::Stop)
        ));
        assert!(matches!(
            settle_failure(&settlement("length", None)),
            FinishReason::Known(FinishReasonKind::MaxTokens)
        ));
        assert!(matches!(
            settle_failure(&settlement("toolUse", None)),
            FinishReason::Known(FinishReasonKind::ToolCalls)
        ));

        let pending = settle_failure(&settlement("pending", None));
        assert_eq!(code_of(&pending), PI_AI_ERROR_CODE);

        let error = settle_failure(&settlement("error", Some("503 upstream")));
        assert_eq!(code_of(&error), "SERVER");
    }
}

// Rust guideline compliant 2026-10-27
