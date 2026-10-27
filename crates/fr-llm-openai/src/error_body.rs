//! Provider HTTP error-body normalization (pi-ai `utils/error-body.ts`,
//! degenerated for the fr face).
//!
//! dsh probes SDK error-object shapes because the `OpenAI` SDK folds bodies
//! under SDK-specific fields. fr has no SDK: the adapter reads the HTTP
//! status and the raw body directly, so [`NormalizedProviderError`]
//! carries them as-is and the SDK-shape probes (`statusCode`/`status`/
//! `$metadata`/`$response` chains, plain-object sniffing, stream-like
//! bodies) are unreachable — noted where the dsh source carries them.

/// The display cap shared by every extracted body (error-body.ts:16).
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// One normalized provider HTTP error: the fields the display-string
/// composer and the classify chain read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProviderError {
    /// The HTTP status code, when one was seen.
    pub status: Option<u16>,
    /// The raw body reason, trimmed and truncated to the cap.
    pub body: Option<String>,
    /// The message text (the SDK-composed string; fr degenerates to the
    /// no-body status text or the body itself).
    pub message: String,
    /// True when `message` already contains the body.
    pub message_carries_body: bool,
}

/// Normalize one HTTP failure (fr degenerate face of
/// `normalizeProviderError`): the status and the raw body text.
#[must_use]
pub fn normalize(status: Option<u16>, body_text: &str) -> NormalizedProviderError {
    let body = extract_body(body_text);
    let message = match (status, body.as_deref()) {
        (Some(status), None) => format!("{status} status code (no body)"),
        (_, Some(body)) => body.to_owned(),
        (None, None) => "(no status code or body)".to_owned(),
    };
    NormalizedProviderError {
        status,
        body,
        message,
        message_carries_body: false,
    }
}

/// The body reason: trimmed, empty dropped, truncated to the cap
/// (error-body.ts `extractBody`; the SDK-shape probe chain is unreachable
/// on the fr face because the raw body arrives directly).
#[must_use]
fn extract_body(body_text: &str) -> Option<String> {
    let trimmed = body_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_error_text(trimmed, MAX_PROVIDER_ERROR_BODY_CHARS))
}

/// Compose the display string (error-body.ts `formatProviderError`, four
/// arms verbatim).
///
/// A body-carrying message or missing status/body yields the message
/// (with the optional prefix when a status exists), otherwise
/// `"<status>: <body>"` with the optional `<prefix> (...)` wrapper.
#[must_use]
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    if norm.message_carries_body || norm.status.is_none() || norm.body.is_none() {
        return match prefix {
            Some(prefix) if norm.status.is_some() => {
                format!(
                    "{} ({}): {}",
                    prefix,
                    norm.status.unwrap_or(0),
                    norm.message
                )
            }
            _ => norm.message.clone(),
        };
    }
    let status = norm.status.unwrap_or(0);
    let body = norm.body.as_deref().unwrap_or_default();
    prefix.map_or_else(
        || format!("{status}: {body}"),
        |prefix| format!("{prefix} ({status}): {body}"),
    )
}

/// Cap one error text (error-body.ts `truncateErrorText` verbatim).
#[must_use]
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max_chars).collect();
    let total = text.chars().count();
    format!("{kept}... [truncated {} chars]", total - max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_pins_the_four_display_arms() {
        let with_body = normalize(Some(403), r#"{"error":{"message":"denied"}}"#);
        assert_eq!(
            format_provider_error(&with_body, None),
            r#"403: {"error":{"message":"denied"}}"#
        );
        assert_eq!(
            format_provider_error(&with_body, Some("OpenAI API error")),
            r#"OpenAI API error (403): {"error":{"message":"denied"}}"#
        );
        let no_body = normalize(Some(400), "   ");
        assert_eq!(no_body.body, None);
        assert_eq!(no_body.message, "400 status code (no body)");
        assert_eq!(
            format_provider_error(&no_body, None),
            "400 status code (no body)"
        );
        assert_eq!(
            format_provider_error(&no_body, Some("OpenAI API error")),
            "OpenAI API error (400): 400 status code (no body)"
        );
    }

    #[test]
    fn truncate_pins_the_cap_and_suffix() {
        let text = "x".repeat(4001);
        let truncated = truncate_error_text(&text, MAX_PROVIDER_ERROR_BODY_CHARS);
        assert!(truncated.starts_with(&"x".repeat(4000)));
        assert!(truncated.ends_with("... [truncated 1 chars]"));
        assert_eq!(truncate_error_text("short", 4000), "short");
    }

    #[test]
    fn normalize_drops_blank_bodies_and_pins_the_no_status_text() {
        let norm = normalize(None, "");
        assert_eq!(norm.status, None);
        assert_eq!(norm.body, None);
        assert_eq!(norm.message, "(no status code or body)");
    }
}
