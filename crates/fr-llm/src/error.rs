//! Provider-neutral error taxonomy (dsh `dsh-llm/error.ts` + `index.ts`
//! `LlmError` construction validation verbatim).

use fr_wire::Failure;
use regex::Regex;
use std::sync::OnceLock;

/// Canonical code for a model request rejected because its context window
/// was exceeded.
///
/// (dsh `CONTEXT_WINDOW_EXCEEDED_CODE`.)
pub const CONTEXT_WINDOW_EXCEEDED_CODE: &str = "CONTEXT_WINDOW_EXCEEDED";

/// Canonical code for an exhausted account quota or balance.
///
/// (dsh `QUOTA_EXCEEDED_CODE`.)
pub const QUOTA_EXCEEDED_CODE: &str = "QUOTA";

/// Account-token quota replenishable through the first-party billing page.
pub const ACCOUNT_QUOTA_EXCEEDED_CODE: &str = "ACCOUNT_QUOTA";

/// Canonical code for a response that completed normally but carried no
/// content blocks at all.
///
/// Retry policy treats it as safe to repeat.
pub const EMPTY_RESPONSE_CODE: &str = "EMPTY_RESPONSE";

/// Canonical code for a credential that was supplied but cannot be used
/// (malformed rather than absent).
///
/// Deliberately outside the default retryable set: a malformed credential
/// fails identically on every attempt.
pub const INVALID_CREDENTIAL_CODE: &str = "INVALID_CREDENTIAL";

/// Canonical code for a request an image-capable route cannot send until
/// more of its images are offloaded.
///
/// The failure's `offload_images` names how many more of the oldest
/// retained occurrences must be offloaded.
pub const IMAGE_OFFLOAD_REQUIRED_CODE: &str = "IMAGE_OFFLOAD_REQUIRED";

/// Canonical code for an authentication failure (401/403 or provider
/// authentication wording).
pub const AUTH_CODE: &str = "AUTH";

/// Canonical code for a request-rate limit (429 or provider rate-limit
/// wording).
pub const RATE_LIMIT_CODE: &str = "RATE_LIMIT";

/// Canonical code for a provider-side server failure (5xx or provider
/// `api_error`/`overloaded` wording).
pub const SERVER_CODE: &str = "SERVER";

/// Canonical code for a stream that exceeded its idle deadline.
pub const TIMEOUT_CODE: &str = "TIMEOUT";

/// Canonical code for a transport-level failure (connection dropped,
/// socket error, truncated stream).
pub const TRANSPORT_CODE: &str = "TRANSPORT";

/// Codes the default (`normal`) retry policy retries (dsh
/// `DEFAULT_RETRYABLE_CODES` in `retry-policy.ts`).
pub const DEFAULT_RETRYABLE_CODES: [&str; 5] = [
    EMPTY_RESPONSE_CODE,
    RATE_LIMIT_CODE,
    SERVER_CODE,
    TIMEOUT_CODE,
    TRANSPORT_CODE,
];

/// Structured context-overflow wording (dsh `STRUCTURED_CONTEXT_OVERFLOW`).
const fn structured_context_overflow() -> &'static str {
    "(?i)(?:^|[^a-z0-9])context[\\s_-](?:length|window)[\\s_-](?:exceed(?:ed|s)?|overflow(?:ed)?|limit[\\s_-]exceeded)(?:$|[^a-z0-9])"
}

/// "Maximum/max context length/window" wording.
const fn max_context_wording() -> &'static str {
    "(?i)(?-u:\\b)(?:maximum|max)(?:\\s+(?:allowed|supported))?\\s+context\\s+(?:length|window)(?-u:\\b)"
}

/// Request-size wording tying "too large" to model context (dsh
/// `TOO_LARGE_FOR_CONTEXT`).
const fn too_large_for_context() -> &'static str {
    "(?i)(?-u:\\b)(?:request|prompt|input|messages?)\\s+(?:is\\s+|are\\s+)?too\\s+(?:large|long)\\s+for\\s+(?:(?:this|the)\\s+)?(?:model(?:'s)?\\s+)?context(?:\\s+window)?(?-u:\\b)"
}

/// "Too long/large for this/the model" wording.
const fn too_long_for_model() -> &'static str {
    "(?i)(?-u:\\b)(?:input|prompt|request)\\s+(?:is\\s+)?too\\s+(?:long|large)\\s+for\\s+(?:this|the)\\s+model(?-u:\\b)"
}

/// "Exceeds" wording safe only when its object is the model context (dsh
/// `EXCEEDS_MODEL_CONTEXT`).
const fn exceeds_model_context() -> &'static str {
    "(?i)(?-u:\\b)(?:input|prompt|request|messages?)(?-u:\\b).{0,40}(?-u:\\b)(?:exceed(?:s|ed)?|overflows?|is\\s+larger\\s+than)(?-u:\\b).{0,40}(?-u:\\b)(?:the\\s+)?(?:model(?:'s)?\\s+)?context(?:\\s+(?:length|window))?(?-u:\\b)"
}

/// The five context-overflow patterns, compiled once.
fn context_patterns() -> &'static [Regex; 5] {
    static PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            Regex::new(structured_context_overflow()).expect("static pattern"),
            Regex::new(max_context_wording()).expect("static pattern"),
            Regex::new(too_large_for_context()).expect("static pattern"),
            Regex::new(too_long_for_model()).expect("static pattern"),
            Regex::new(exceeds_model_context()).expect("static pattern"),
        ]
    })
}

/// Recognize the context-overflow wording used by OpenAI-compatible
/// providers and library adapters.
///
/// Adapters pass all available provider code, type, and message text so both
/// thrown and in-band delivery styles share one classifier (dsh
/// `isContextWindowExceededError`).
#[must_use]
pub fn is_context_window_exceeded_error(detail: &str) -> bool {
    context_patterns()
        .iter()
        .any(|pattern| pattern.is_match(detail))
}

/// The five terminal-quota patterns, compiled once (dsh
/// `isQuotaExceededError`).
fn quota_patterns() -> &'static [Regex; 5] {
    static PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            Regex::new("(?i)(?-u:\\b)insufficient[\\s_-]+(?:quota|balance|credits?)(?-u:\\b)").expect("static pattern"),
            Regex::new("(?i)(?-u:\\b)(?:quota|usage[\\s_-]+limit)[\\s_-]+(?:exceeded|exhausted|reached)(?-u:\\b)").expect("static pattern"),
            Regex::new("(?i)(?-u:\\b)exceed(?:ed|s)?[\\s_-]+(?:(?:your|the)[\\s_-]+)?(?:current[\\s_-]+)?quota(?-u:\\b)").expect("static pattern"),
            Regex::new("(?i)(?-u:\\b)(?:balance|credits?)[\\s_-]+(?:exhausted|depleted)(?-u:\\b)").expect("static pattern"),
            Regex::new("(?i)(?-u:\\b)out[\\s_-]+of[\\s_-]+(?:credits?|budget)(?-u:\\b)").expect("static pattern"),
        ]
    })
}

/// Recognize provider wording that identifies an exhausted account quota
/// rather than a transient request-rate limit: terminal quota, balance,
/// credit, budget, or usage-limit wording only.
#[must_use]
pub fn is_quota_exceeded_error(detail: &str) -> bool {
    quota_patterns()
        .iter()
        .any(|pattern| pattern.is_match(detail))
}

/// Why [`LlmError::try_new`] rejected an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstructionRejection {
    /// The human-readable summary was empty.
    EmptyMessage,
    /// The machine-routing code was empty.
    EmptyCode,
    /// The HTTP status was outside `100..=599`.
    StatusOutOfRange(i64),
    /// The provider retry hint was zero or negative.
    NonPositiveRetryAfter,
    /// The provider request id was empty.
    EmptyRequestId,
}

impl std::fmt::Display for ConstructionRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyMessage => write!(f, "LlmError message must be a non-empty string"),
            Self::EmptyCode => write!(f, "LlmError code must be a non-empty string"),
            Self::StatusOutOfRange(status) => {
                write!(
                    f,
                    "LlmError status must be an integer from 100 through 599, got {status}"
                )
            }
            Self::NonPositiveRetryAfter => {
                write!(
                    f,
                    "LlmError providerRetryAfterMs must be a positive finite number"
                )
            }
            Self::EmptyRequestId => write!(f, "LlmError requestId must be a non-empty string"),
        }
    }
}

impl std::error::Error for ConstructionRejection {}

/// A provider-neutral model-request failure with a stable machine-routable
/// code (dsh `LlmError`): route on `code`, never by parsing the message.
///
/// Construction validates exactly the five facts dsh validates (dsh
/// `index.ts:96-131`).
///
/// `offload_images` pairing with [`IMAGE_OFFLOAD_REQUIRED_CODE`] is a
/// type-documentation constraint, and its positive-integer check belongs to
/// the adapter-failure snapshot path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmError {
    failure: Failure,
}

impl LlmError {
    /// Build a validated failure.
    ///
    /// # Errors
    /// [`ConstructionRejection`] when the message or code is empty, the
    /// status is outside `100..=599`, the retry hint is not positive, or the
    /// request id is empty.
    pub fn try_new(
        message: &str,
        code: &str,
        status: Option<u16>,
        provider_retry_after_ms: Option<u64>,
        request_id: Option<&str>,
        offload_images: Option<u64>,
    ) -> Result<Self, ConstructionRejection> {
        if message.is_empty() {
            return Err(ConstructionRejection::EmptyMessage);
        }
        if code.is_empty() {
            return Err(ConstructionRejection::EmptyCode);
        }
        if let Some(status) = status {
            let wide = i64::from(status);
            if !(100..=599).contains(&wide) {
                return Err(ConstructionRejection::StatusOutOfRange(wide));
            }
        }
        // `provider_retry_after_ms` is `u64`: negative and non-finite values
        // are unrepresentable at the type level, mirroring dsh's positive
        // finite validation.
        if provider_retry_after_ms == Some(0) {
            return Err(ConstructionRejection::NonPositiveRetryAfter);
        }
        if request_id.is_some_and(str::is_empty) {
            return Err(ConstructionRejection::EmptyRequestId);
        }
        Ok(Self {
            failure: Failure {
                message: message.to_owned(),
                code: code.to_owned(),
                status,
                provider_retry_after_ms,
                request_id: request_id.map(fr_wire::ProviderRequestId::new),
                offload_images,
            },
        })
    }

    /// The serializable failure facts.
    #[must_use]
    pub const fn failure(&self) -> &Failure {
        &self.failure
    }

    /// The stable machine-routable code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.failure.code
    }

    /// The human-readable failure summary.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.failure.message
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.failure.message)
    }
}

impl std::error::Error for LlmError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_error_construction_validates_fields() {
        // Empty message / empty code.
        assert!(matches!(
            LlmError::try_new("", "AUTH", None, None, None, None),
            Err(ConstructionRejection::EmptyMessage)
        ));
        assert!(matches!(
            LlmError::try_new("m", "", None, None, None, None),
            Err(ConstructionRejection::EmptyCode)
        ));
        // Status bounds.
        assert!(matches!(
            LlmError::try_new("m", "AUTH", Some(99), None, None, None),
            Err(ConstructionRejection::StatusOutOfRange(99))
        ));
        assert!(matches!(
            LlmError::try_new("m", "AUTH", Some(600), None, None, None),
            Err(ConstructionRejection::StatusOutOfRange(600))
        ));
        // Retry hint: negative and non-finite values are unrepresentable at
        // the u64 type level (type-level rejection mirrors dsh's positive
        // finite validation); zero is the representable non-positive value.
        assert!(matches!(
            LlmError::try_new("m", "AUTH", None, Some(0), None, None),
            Err(ConstructionRejection::NonPositiveRetryAfter)
        ));
        // Empty request id.
        assert!(matches!(
            LlmError::try_new("m", "AUTH", None, None, Some(""), None),
            Err(ConstructionRejection::EmptyRequestId)
        ));
        // A legal construction retains every field.
        let error = LlmError::try_new(
            "boom",
            IMAGE_OFFLOAD_REQUIRED_CODE,
            Some(429),
            Some(1_500),
            Some("req-1"),
            Some(3),
        )
        .expect("legal construction");
        assert_eq!(error.code(), IMAGE_OFFLOAD_REQUIRED_CODE);
        assert_eq!(error.failure().provider_retry_after_ms, Some(1_500));
        assert_eq!(error.failure().offload_images, Some(3));
    }

    #[test]
    fn context_window_and_quota_wording_detected() {
        // Context-overflow families (one per pattern).
        assert!(is_context_window_exceeded_error("context_length_exceeded"));
        assert!(is_context_window_exceeded_error(
            "This model's maximum context length is 8192 tokens"
        ));
        assert!(is_context_window_exceeded_error(
            "prompt is too large for context window"
        ));
        assert!(is_context_window_exceeded_error(
            "input is too long for this model"
        ));
        assert!(is_context_window_exceeded_error(
            "your request exceeded the model context window"
        ));
        assert!(!is_context_window_exceeded_error(
            "request timeout while reading body"
        ));
        // Terminal quota families (one per pattern).
        assert!(is_quota_exceeded_error("Insufficient Balance"));
        assert!(is_quota_exceeded_error("quota exceeded for this project"));
        assert!(is_quota_exceeded_error("You exceeded your current quota"));
        assert!(is_quota_exceeded_error("credits exhausted"));
        assert!(is_quota_exceeded_error("You are out of budget"));
        assert!(!is_quota_exceeded_error(
            "rate limit exceeded, retry after 5s"
        ));
    }
}
