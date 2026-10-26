//! Normalize HTTP and in-band Messages errors into provider-neutral
//! failures (dsh `transport.ts`, classification chain and diagnostics
//! verbatim).

use std::borrow::Cow;

use fr_llm::error::{
    AUTH_CODE, CONTEXT_WINDOW_EXCEEDED_CODE, LlmError, QUOTA_EXCEEDED_CODE, RATE_LIMIT_CODE,
    SERVER_CODE, is_context_window_exceeded_error, is_quota_exceeded_error,
};

use crate::codes::INVALID_REQUEST_CODE;

/// Look up one response header by name (fetch `headers.get(name)`
/// semantics; case-insensitivity belongs to the real transport).
pub type HeaderLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The no-headers lookup used when classification runs without a response
/// face (in-band error events).
const fn no_header(_: &str) -> Option<String> {
    None
}

/// Read only provider error fields used by bounded Files recovery
/// (dsh `providerErrorDetail`).
#[must_use]
pub fn provider_error_detail(raw: &serde_json::Value) -> String {
    let error = raw.get("error");
    let Some(error) = error else {
        return String::new();
    };
    let Some(map) = error.as_object() else {
        return String::new();
    };
    ["code", "type", "message"]
        .iter()
        .filter_map(|field| map.get(*field).and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) to Unix epoch
/// milliseconds. dsh delegates to `Date.parse`, which also reads obsolete
/// RFC 850 and asctime forms; this port narrows to the one format RFC 9110
/// marks preferred (the numeric `retry-after` form covers the observable
/// contract in practice), and any other text yields `None` like a garbage
/// `Date.parse` result.
#[must_use]
fn parse_http_date_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    let rest = text.strip_suffix(" GMT")?;
    // Drop the leading weekday (`Fri, `); the GMT suffix plus a comma-
    // prefixed token are the format's only decoration.
    let rest = rest.split_once(", ").map_or(rest, |(_, rest)| rest);
    let mut parts = rest.split(' ');
    let day: i64 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    let month = match month_name {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let (hour, minute, second) = scan_time(time)?;
    Some(
        days_from_civil(year, month, day) * 86_400_000
            + (hour * 3_600 + minute * 60 + second) * 1_000,
    )
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = i64::from(month <= 2) * (year - 1) + i64::from(month > 2) * year;
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Scan `hh:mm:ss` from the tail of an IMF-fixdate.
fn scan_time(time: &str) -> Option<(i64, i64, i64)> {
    let mut fields = time.split(':');
    let hour: i64 = fields.next()?.parse().ok()?;
    let minute: i64 = fields.next()?.parse().ok()?;
    let second: i64 = fields.next()?.parse().ok()?;
    Some((hour, minute, second))
}

/// Numeric `retry-after` text (`/^\d+(?:\.\d+)?$/`), or `None`.
fn numeric_retry_after(text: &str) -> Option<f64> {
    let digits = |slice: &[u8]| !slice.is_empty() && slice.iter().all(u8::is_ascii_digit);
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (text, ""),
    };
    if digits(whole.as_bytes()) && (fraction.is_empty() || digits(fraction.as_bytes())) {
        text.parse().ok()
    } else {
        None
    }
}

/// Extract one string field from the `error` envelope, defaulting to `""`.
fn error_field<'a>(raw: &'a serde_json::Value, field: &str) -> &'a str {
    raw.get("error")
        .and_then(|error| error.get(field))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Classify a provider error without trusting arbitrary response fields
/// (dsh `providerError`).
///
/// Chain order: `AUTH` → `QUOTA` → `RATE_LIMIT` → `CONTEXT_WINDOW_EXCEEDED`
/// → `INVALID_REQUEST` → `SERVER` → `HTTP_<status>`; `retry-after` and
/// request-id headers read through `headers`.
///
/// `now_ms` is the wall clock the real transport injects (dsh reads
/// `Date.now()` inside); the sub-millisecond fraction of a numeric
/// `retry-after` floors to `u64`, a sub-frame rounding dsh's float keeps
/// but the failure shape (`u64` ms) cannot represent.
#[must_use]
pub fn provider_error(
    raw: &serde_json::Value,
    status: Option<u16>,
    headers: Option<HeaderLookup<'_>>,
    now_ms: i64,
) -> LlmError {
    let text = error_field(raw, "message");
    let message = if text.is_empty() {
        format!(
            "DeepSeek Messages request failed ({})",
            status.map_or_else(|| "stream error".to_string(), |status| status.to_string())
        )
    } else {
        text.to_string()
    };
    let kind = error_field(raw, "type");
    let code_text = error_field(raw, "code");
    let detail = format!("{kind} {code_text} {message}");
    let text = |values: &[&str]| values.contains(&kind);
    let code: std::borrow::Cow<'static, str> = if status == Some(401)
        || status == Some(403)
        || text(&["authentication_error", "permission_error"])
    {
        Cow::Borrowed(AUTH_CODE)
    } else if is_quota_exceeded_error(&detail) || status == Some(402) {
        Cow::Borrowed(QUOTA_EXCEEDED_CODE)
    } else if status == Some(429) || kind == "rate_limit_error" {
        Cow::Borrowed(RATE_LIMIT_CODE)
    } else if is_context_window_exceeded_error(&detail) {
        Cow::Borrowed(CONTEXT_WINDOW_EXCEEDED_CODE)
    } else if status == Some(400) || status == Some(413) || kind == "invalid_request_error" {
        Cow::Borrowed(INVALID_REQUEST_CODE)
    } else if status.is_some_and(|status| status >= 500) || text(&["api_error", "overloaded_error"])
    {
        Cow::Borrowed(SERVER_CODE)
    } else {
        status.map_or(Cow::Borrowed(SERVER_CODE), |status| {
            Cow::Owned(format!("HTTP_{status}"))
        })
    };
    build_error(&message, &code, status, headers, now_ms)
}

/// Assemble the [`LlmError`] payload: retry-after (`/^\d+(\.\d+)?$/` times
/// 1000, or an IMF-fixdate minus the injected clock) and the request id
/// read in `request-id` → `x-request-id` → `x-deepseek-request-id` order.
fn build_error(
    message: &str,
    code: &str,
    status: Option<u16>,
    headers: Option<HeaderLookup<'_>>,
    now_ms: i64,
) -> LlmError {
    let lookup: HeaderLookup<'_> = headers.unwrap_or(&no_header);
    let retry = lookup("retry-after");
    let delay = retry
        .as_deref()
        .map_or_else(
            || None,
            |retry| {
                numeric_retry_after(retry).map_or_else(
                    || parse_http_date_ms(retry).and_then(|ms| u64::try_from(ms - now_ms).ok()),
                    |seconds| {
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "numeric retry-after matched /^\\\\d+(\\\\.\\\\d+)?$/ so \
                                      seconds >= 0; the sub-millisecond fraction floors, \
                                      which the u64 failure shape cannot represent"
                        )]
                        let millis = (seconds * 1000.0) as u64;
                        Some(millis)
                    },
                )
            },
        )
        .filter(|delay| *delay > 0);
    let id = ["request-id", "x-request-id", "x-deepseek-request-id"]
        .iter()
        .find_map(|name| lookup(name))
        .filter(|id| !id.is_empty());
    // `try_new` re-validates every field; the values here satisfy its five
    // invariants by construction (non-empty message/code, in-range status,
    // positive delay, non-empty id).
    LlmError::try_new(message, code, status, delay, id.as_deref(), None)
        .expect("classified provider error fields satisfy the LlmError invariants")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::ids::ProviderRequestId;
    use serde_json::json;

    /// Builds a header lookup from literal pairs, standing in for the real
    /// transport's case-folding map.
    fn lookup<'p>(pairs: &'p [(&'p str, &'p str)]) -> impl Fn(&str) -> Option<String> + 'p {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    fn code_of(err: &LlmError) -> &str {
        err.code()
    }

    #[test]
    fn classification_chain_matches_dsh_order() {
        let none = serde_json::Value::Null;
        let typed = |kind: &str| json!({"error": {"type": kind, "message": "m"}});
        assert_eq!(code_of(&provider_error(&none, Some(401), None, 0)), "AUTH");
        assert_eq!(code_of(&provider_error(&none, Some(403), None, 0)), "AUTH");
        assert_eq!(
            code_of(&provider_error(
                &typed("authentication_error"),
                None,
                None,
                0
            )),
            "AUTH"
        );
        assert_eq!(
            code_of(&provider_error(&typed("permission_error"), None, None, 0)),
            "AUTH"
        );
        assert_eq!(code_of(&provider_error(&none, Some(402), None, 0)), "QUOTA");
        assert_eq!(
            code_of(&provider_error(&none, Some(429), None, 0)),
            "RATE_LIMIT"
        );
        assert_eq!(
            code_of(&provider_error(&typed("rate_limit_error"), None, None, 0)),
            "RATE_LIMIT"
        );
        let overflow =
            json!({"error": {"message": "This model's maximum context length is 4096 tokens"}});
        assert_eq!(
            code_of(&provider_error(&overflow, None, None, 0)),
            "CONTEXT_WINDOW_EXCEEDED"
        );
        // Wording outside the five-pattern family falls through (dsh-true).
        let other = json!({"error": {"message": "prompt is too long: 1000 tokens > 500 maximum"}});
        assert_eq!(code_of(&provider_error(&other, None, None, 0)), "SERVER");
        assert_eq!(
            code_of(&provider_error(&none, Some(400), None, 0)),
            "INVALID_REQUEST"
        );
        assert_eq!(
            code_of(&provider_error(&none, Some(413), None, 0)),
            "INVALID_REQUEST"
        );
        assert_eq!(
            code_of(&provider_error(
                &typed("invalid_request_error"),
                None,
                None,
                0
            )),
            "INVALID_REQUEST"
        );
        assert_eq!(
            code_of(&provider_error(&none, Some(500), None, 0)),
            "SERVER"
        );
        assert_eq!(
            code_of(&provider_error(&typed("api_error"), None, None, 0)),
            "SERVER"
        );
        assert_eq!(
            code_of(&provider_error(&typed("overloaded_error"), None, None, 0)),
            "SERVER"
        );
        assert_eq!(code_of(&provider_error(&none, None, None, 0)), "SERVER");
        assert_eq!(
            code_of(&provider_error(&none, Some(418), None, 0)),
            "HTTP_418"
        );
        // Precedence: AUTH wins over quota wording on 401.
        let mixed =
            json!({"error": {"type": "authentication_error", "message": "exceeded your quota"}});
        assert_eq!(code_of(&provider_error(&mixed, Some(401), None, 0)), "AUTH");
    }

    #[test]
    fn retry_after_and_request_id_headers_pin_dsh_reading() {
        let raw = serde_json::Value::Null;
        let future = "Fri, 01 Jan 2038 00:00:00 GMT";
        let now = 1_800_000_000_000_i64; // 2027-01-15, before the pinned date.
        let err = provider_error(
            &raw,
            Some(429),
            Some(&lookup(&[("retry-after", "12")])),
            now,
        );
        assert_eq!(err.failure().provider_retry_after_ms, Some(12_000));
        let err = provider_error(
            &raw,
            Some(429),
            Some(&lookup(&[("retry-after", "1.5")])),
            now,
        );
        assert_eq!(err.failure().provider_retry_after_ms, Some(1_500));
        let err = provider_error(
            &raw,
            Some(429),
            Some(&lookup(&[("retry-after", "soon")])),
            now,
        );
        assert_eq!(err.failure().provider_retry_after_ms, None);
        let pairs = [("retry-after", future)];
        let err = provider_error(&raw, Some(429), Some(&lookup(&pairs)), now);
        let expected = u64::try_from(2_145_916_800_000_i64 - now).ok();
        assert_eq!(err.failure().provider_retry_after_ms, expected);
        let past = "Thu, 01 Jan 1970 00:00:00 GMT";
        let err = provider_error(
            &raw,
            Some(429),
            Some(&lookup(&[("retry-after", past)])),
            now,
        );
        assert_eq!(err.failure().provider_retry_after_ms, None);
        let ids = lookup(&[
            ("request-id", "first"),
            ("x-request-id", "second"),
            ("x-deepseek-request-id", "third"),
        ]);
        let err = provider_error(&raw, None, Some(&ids), now);
        assert_eq!(
            err.failure()
                .request_id
                .as_ref()
                .map(ProviderRequestId::as_str),
            Some("first")
        );
        let only_x = lookup(&[("x-deepseek-request-id", "third")]);
        let err = provider_error(&raw, None, Some(&only_x), now);
        assert_eq!(
            err.failure()
                .request_id
                .as_ref()
                .map(ProviderRequestId::as_str),
            Some("third")
        );
    }

    #[test]
    fn detail_and_default_message_pin_dsh_texts() {
        let raw = json!({"error": {"code": "insufficient_quota", "type": "quota_error", "message": "nope", "extra": 1}});
        assert_eq!(
            provider_error_detail(&raw),
            "insufficient_quota quota_error nope"
        );
        assert_eq!(provider_error_detail(&json!({"error": {"code": 1}})), "");
        assert_eq!(provider_error_detail(&json!({})), "");
        let err = provider_error(&json!({}), Some(404), None, 0);
        assert_eq!(err.message(), "DeepSeek Messages request failed (404)");
        let err = provider_error(&json!({}), None, None, 0);
        assert_eq!(
            err.message(),
            "DeepSeek Messages request failed (stream error)"
        );
    }
}
