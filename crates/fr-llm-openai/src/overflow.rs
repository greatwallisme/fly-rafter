//! Context-overflow detection (pi-ai `utils/overflow.ts`, verbatim
//! pattern family).
//!
//! Three cases: an error message matching the provider pattern family, a
//! silent `stop` whose usage already exceeds the window (`z.ai` style), and
//! a zero-output `length` that fills the window (`Xiaomi MiMo` style).
//!
//! The `isRecoverableLength` helper stays unported: the recovery layer
//! that consumes it lands in a later slice.

use std::sync::OnceLock;

/// The overflow pattern family (overflow.ts:37-63, verbatim; JS `/i` maps
/// to the inline `(?i)` flag).
const OVERFLOW_PATTERNS: [&str; 25] = [
    r"prompt (?:is )?too long",
    r"prompt exceeds max length",
    r"request_too_large",
    r"input is too long for requested model",
    r"exceeds the context window",
    r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))",
    r"input token count.*exceeds the maximum",
    r"maximum prompt length is \d+",
    r"reduce the length of the messages",
    r"maximum context length is \d+ tokens",
    r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",
    r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)",
    r"exceeds the limit of \d+",
    r"exceeds the available context size",
    r"greater than the context length",
    r"context window exceeds limit",
    r"exceeded model token limit",
    r"too large for model with \d+ maximum context length",
    r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
    r"model_context_window_exceeded",
    r"prompt too long; exceeded (?:max )?context length",
    r"range of input length should be",
    r"context[_ ]length[_ ]exceeded",
    r"too many tokens",
    r"token limit exceeded",
];

/// Non-overflow exclusions (rate limiting and the Bedrock human-readable
/// prefixes that would otherwise hit the generic family).
const NON_OVERFLOW_PATTERNS: [&str; 3] = [
    r"^(Throttling error|Service unavailable):",
    r"rate limit",
    r"too many requests",
];

/// The Cerebras bodyless status text (the `OpenAI` SDK's
/// `"400 status code (no body)"` family, gated on `provider ==
/// "cerebras"`).
const CEREBRAS_BODYLESS_OVERFLOW_PATTERN: &str = r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)";

fn patterns() -> &'static Vec<regex::Regex> {
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        OVERFLOW_PATTERNS
            .iter()
            .map(|pattern| regex::Regex::new(&format!("(?i){pattern}")).expect("overflow pattern"))
            .collect()
    })
}

fn non_overflow_patterns() -> &'static Vec<regex::Regex> {
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        NON_OVERFLOW_PATTERNS
            .iter()
            .map(|pattern| {
                regex::Regex::new(&format!("(?i){pattern}")).expect("non-overflow pattern")
            })
            .collect()
    })
}

fn cerebras_bodyless() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(&format!("(?i){CEREBRAS_BODYLESS_OVERFLOW_PATTERN}"))
            .expect("cerebras pattern")
    })
}

/// The stop reason face the three overflow cases read (the fr settlement
/// facts instead of the dsh `AssistantMessage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowStopKind {
    /// The provider reported an error stop.
    Error,
    /// A natural stop.
    Stop,
    /// The output-token ceiling.
    Length,
}

/// Whether one settled response indicates a context overflow
/// (`overflow.ts` `isContextOverflow`, three cases verbatim).
///
/// The usage input already folds the cache-read count (dsh
/// `input + cacheRead`).
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "dsh compares these counts as JS Numbers (f64); token counts stay far below 2^53 so the cast is exact"
)]
pub fn is_context_overflow(
    stop: OverflowStopKind,
    error_message: Option<&str>,
    provider: &str,
    input_tokens: u64,
    output_tokens: u64,
    context_window: Option<u64>,
) -> bool {
    // Case 1: error-message patterns, minus the non-overflow exclusions.
    if stop == OverflowStopKind::Error
        && let Some(message) = error_message
    {
        let is_non_overflow = non_overflow_patterns().iter().any(|p| p.is_match(message));
        if !is_non_overflow
            && (patterns().iter().any(|p| p.is_match(message))
                || (provider == "cerebras" && cerebras_bodyless().is_match(message)))
        {
            return true;
        }
    }
    let Some(window) = context_window else {
        return false;
    };
    // Case 2: silent overflow — a successful stop above the window.
    if stop == OverflowStopKind::Stop && input_tokens as f64 > window as f64 {
        return true;
    }
    // Case 3: length-stop overflow — zero output on a filled window
    // (dsh compares against `contextWindow * 0.99` in float).
    stop == OverflowStopKind::Length
        && output_tokens == 0
        && input_tokens as f64 >= window as f64 * 0.99
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented provider examples: Anthropic token overflow, `OpenAI`
    /// window text, `LiteLLM` spelling, `xAI`, and the 413 `request_too_large`.
    #[test]
    fn error_patterns_pin_the_provider_family() {
        let hits = [
            "prompt is too long: 213462 tokens > 200000 maximum",
            "Your input exceeds the context window of this model",
            "Requested token count exceeds the model's maximum context length of 131072 tokens",
            "This model's maximum prompt length is 131072 but the request contains 537812 tokens",
            "413 {\"error\":{\"type\":\"request_too_large\",\"message\":\"Request exceeds the maximum size\"}}",
            "Input length (265330) exceeds model's maximum context length (262144).",
            "prompt too long; exceeded max context length",
        ];
        for message in hits {
            assert!(
                is_context_overflow(OverflowStopKind::Error, Some(message), "openai", 0, 0, None),
                "expected overflow: {message}"
            );
        }
    }

    /// The Bedrock throttling exclusion, the generic rate-limit exclusions,
    /// and the cerebras-only gate on the bodyless status text.
    #[test]
    fn non_overflow_exclusions_and_cerebras_gate_pin() {
        assert!(!is_context_overflow(
            OverflowStopKind::Error,
            Some("Throttling error: too many tokens, please wait before trying again."),
            "openai",
            0,
            0,
            None
        ));
        assert!(!is_context_overflow(
            OverflowStopKind::Error,
            Some("400 status code (no body)"),
            "openai",
            0,
            0,
            None
        ));
        assert!(is_context_overflow(
            OverflowStopKind::Error,
            Some("400 status code (no body)"),
            "cerebras",
            0,
            0,
            None
        ));
        assert!(is_context_overflow(
            OverflowStopKind::Error,
            Some("413 status code (no body)"),
            "cerebras",
            0,
            0,
            None
        ));
    }

    /// Case 2 (silent stop above the window) and case 3 (zero-output
    /// length at >= 0.99 of the window), plus the no-window gates.
    #[test]
    fn silent_and_length_cases_pin_the_usage_thresholds() {
        let window = Some(1_000);
        assert!(is_context_overflow(
            OverflowStopKind::Stop,
            None,
            "zai",
            1_001,
            10,
            window
        ));
        assert!(!is_context_overflow(
            OverflowStopKind::Stop,
            None,
            "zai",
            1_000,
            10,
            window
        ));
        assert!(is_context_overflow(
            OverflowStopKind::Length,
            None,
            "mimo",
            1_000,
            0,
            window
        ));
        assert!(!is_context_overflow(
            OverflowStopKind::Length,
            None,
            "mimo",
            980,
            0,
            window
        ));
        assert!(!is_context_overflow(
            OverflowStopKind::Length,
            None,
            "mimo",
            1_000,
            5,
            window
        ));
        assert!(!is_context_overflow(
            OverflowStopKind::Stop,
            None,
            "zai",
            1_001,
            10,
            None
        ));
    }
}

// Rust guideline compliant 2026-10-27
