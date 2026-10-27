//! The anthropic provider error codes (the dsh `codes.ts` analog for the
//! `anthropic-messages` protocol face).
//!
//! Codes stay verbatim to the dsh behavior surface (Q76-B: adapted
//! wording, verbatim code values), including the `PI_AI_ERROR` fallback
//! whose value names the library the behavior is pinned to.

/// An unclassified provider failure (the classify-chain fallback).
pub const PI_AI_ERROR_CODE: &str = "PI_AI_ERROR";

/// The request names a model the route does not declare (Q57-B: no
/// built-in catalog, hand-declared models only).
pub const UNKNOWN_MODEL_CODE: &str = "UNKNOWN_MODEL";

/// The adapter does not own the provider route.
pub const NO_ADAPTER_CODE: &str = "NO_ADAPTER";

/// A rejected request body (gateway or provider size cap) or a 400-family
/// text: resending the same request cannot succeed.
pub const INVALID_REQUEST_CODE: &str = "INVALID_REQUEST";

/// A profile-level configuration failure (stored model errors).
pub const INVALID_CONFIG_CODE: &str = "INVALID_CONFIG";

/// `GenerateOptions.stop` is not supported by the pi-ai-face adapters.
pub const UNSUPPORTED_OPTION_CODE: &str = "UNSUPPORTED_OPTION";

/// Image input on a model that does not declare it, or with the
/// attachment seam absent.
pub const UNSUPPORTED_CONTENT_CODE: &str = "UNSUPPORTED_CONTENT";

/// The requested reasoning effort is not in the model's supported set.
pub const UNSUPPORTED_REASONING_EFFORT_CODE: &str = "UNSUPPORTED_REASONING_EFFORT";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_values_pin_the_dsh_surface() {
        assert_eq!(PI_AI_ERROR_CODE, "PI_AI_ERROR");
        assert_eq!(UNKNOWN_MODEL_CODE, "UNKNOWN_MODEL");
        assert_eq!(NO_ADAPTER_CODE, "NO_ADAPTER");
        assert_eq!(INVALID_REQUEST_CODE, "INVALID_REQUEST");
        assert_eq!(INVALID_CONFIG_CODE, "INVALID_CONFIG");
        assert_eq!(UNSUPPORTED_OPTION_CODE, "UNSUPPORTED_OPTION");
        assert_eq!(UNSUPPORTED_CONTENT_CODE, "UNSUPPORTED_CONTENT");
        assert_eq!(
            UNSUPPORTED_REASONING_EFFORT_CODE,
            "UNSUPPORTED_REASONING_EFFORT"
        );
    }
}
// Rust guideline compliant 2026-10-27
