//! Chunk usage parsing and finish-reason mapping (pi-ai
//! `api/openai-completions.ts:1519-1586`).

use serde_json::Value;

use fr_llm::error::SERVER_CODE;
use fr_wire::RawObject;
use fr_wire::call_config::{Failure, FinishReason, FinishReasonKind, TokenUsage};

/// A JSON number field under the JS `x || 0` truthiness: `0`, `null`, and
/// absence all yield `0`. Providers do not report string counts; non-number
/// JSON values are treated as absent (a degenerate-garbage case where JS
/// would propagate `NaN`).
fn truthy_count(raw: &RawObject, key: &str) -> f64 {
    raw.get(key)
        .and_then(Value::as_f64)
        .filter(|value| *value != 0.0)
        .unwrap_or(0.0)
}

/// Safe-integer window (`Number.isSafeInteger` bound); the deepseek
/// `update_usage` policy.
const SAFE_INTEGER_LIMIT: f64 = 9_007_199_254_740_991.0;

fn non_negative_u64(value: f64) -> Option<u64> {
    if value.fract() != 0.0 || !(0.0..=SAFE_INTEGER_LIMIT).contains(&value) {
        // Degenerate garbage (negative or fractional counts) has no durable
        // representation; the field is dropped.
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "fract()==0 and the 0..=2^53-1 window are checked above, so the cast is exact and non-negative"
    )]
    Some(value as u64)
}

/// One `ChatCompletionChunk.usage` mapped to the durable accounting
/// (`parseChunkUsage`).
///
/// Cache reads fall back through `prompt_tokens_details.cached_tokens`,
/// `prompt_cache_hit_tokens` (`DeepSeek`), and top-level `cached_tokens`
/// (`Kimi`) — the JS `??` chain passes a present `0` through, so `Option`
/// presence, not truthiness, drives it. Writes ride
/// `prompt_tokens_details.cache_write_tokens`. Inputs are uncached, outputs
/// already include reasoning tokens, and the total is the four-way sum; cost
/// stays zero (D7, `NO_COST`).
#[must_use]
pub fn parse_chunk_usage(raw: &RawObject) -> TokenUsage {
    let prompt_tokens = truthy_count(raw, "prompt_tokens");
    let cache_read_tokens = raw
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_f64)
        .or_else(|| raw.get("prompt_cache_hit_tokens").and_then(Value::as_f64))
        .or_else(|| raw.get("cached_tokens").and_then(Value::as_f64))
        .unwrap_or(0.0);
    let cache_write_tokens = raw
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cache_write_tokens"))
        .and_then(Value::as_f64)
        .filter(|value| *value != 0.0)
        .unwrap_or(0.0);
    let input = (prompt_tokens - cache_read_tokens - cache_write_tokens).max(0.0);
    let output_tokens = truthy_count(raw, "completion_tokens");
    let reasoning_tokens = raw
        .get("completion_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_f64)
        .filter(|value| *value != 0.0)
        .unwrap_or(0.0);
    let total_tokens = input + output_tokens + cache_read_tokens + cache_write_tokens;
    TokenUsage {
        input_tokens: non_negative_u64(input).unwrap_or(0),
        output_tokens: non_negative_u64(output_tokens).unwrap_or(0),
        total_tokens: non_negative_u64(total_tokens),
        cache_read_tokens: non_negative_u64(cache_read_tokens),
        cache_write_tokens: non_negative_u64(cache_write_tokens),
        reasoning_tokens: non_negative_u64(reasoning_tokens),
    }
}

/// One `choice.finish_reason` mapped to the terminal reason
/// (`mapStopReason`).
///
/// `null` is a natural stop, `end` is a provider synonym for `stop`,
/// `function_call` is the legacy tool arm, and anything unmapped becomes an
/// error stop carrying the verbatim `Provider finish_reason: <reason>`
/// message. The fr `Failure.code` has no dsh counterpart (dsh `StopReason`
/// carries only the message); `SERVER` is the neutral machine-routing class
/// for provider-side failures.
#[must_use]
pub fn map_stop_reason(finish_reason: Option<&str>) -> FinishReason {
    match finish_reason {
        None | Some("stop" | "end") => FinishReason::Known(FinishReasonKind::Stop),
        Some("length") => FinishReason::Known(FinishReasonKind::MaxTokens),
        Some("function_call" | "tool_calls") => FinishReason::Known(FinishReasonKind::ToolCalls),
        Some(reason) => FinishReason::Known(FinishReasonKind::Error {
            failure: Failure {
                message: format!("Provider finish_reason: {reason}"),
                code: SERVER_CODE.to_owned(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
                offload_images: None,
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(body: serde_json::Value) -> TokenUsage {
        parse_chunk_usage(&serde_json::from_value::<RawObject>(body).expect("object"))
    }

    /// The three provider placements of cache reads (`OpenAI` details,
    /// `DeepSeek` top-level hit counter, `Kimi` legacy `cached_tokens`) and
    /// the input subtraction with the max(0, ...) clamp.
    #[test]
    fn maps_cache_read_placements_and_clamps() {
        let openai = usage(json!({
            "prompt_tokens": 100,
            "completion_tokens": 30,
            "prompt_tokens_details": {"cached_tokens": 60, "cache_write_tokens": 5},
            "completion_tokens_details": {"reasoning_tokens": 10},
        }));
        assert_eq!(openai.cache_read_tokens, Some(60));
        assert_eq!(openai.cache_write_tokens, Some(5));
        assert_eq!(openai.input_tokens, 35);
        assert_eq!(openai.output_tokens, 30);
        assert_eq!(openai.reasoning_tokens, Some(10));
        // The dsh total is the four-way sum over uncached input.
        assert_eq!(openai.total_tokens, Some(130));

        let deepseek = usage(json!({
            "prompt_tokens": 50,
            "completion_tokens": 20,
            "prompt_cache_hit_tokens": 40,
        }));
        assert_eq!(deepseek.cache_read_tokens, Some(40));
        assert_eq!(deepseek.input_tokens, 10);

        let kimi = usage(json!({
            "prompt_tokens": 50,
            "completion_tokens": 20,
            "cached_tokens": 45,
        }));
        assert_eq!(kimi.cache_read_tokens, Some(45));
        assert_eq!(kimi.input_tokens, 5);

        // A zero counts as present for the `??` chain but falsy for `||`.
        let zero = usage(json!({
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "prompt_tokens_details": {"cached_tokens": 0},
        }));
        assert_eq!(zero.cache_read_tokens, Some(0));
        assert_eq!(zero.input_tokens, 0);
        assert_eq!(zero.total_tokens, Some(0));

        // Reads exceeding the prompt clamp the uncached input at zero.
        let over = usage(json!({
            "prompt_tokens": 10,
            "completion_tokens": 4,
            "cached_tokens": 20,
        }));
        assert_eq!(over.input_tokens, 0);
        // Four-way sum: 0 input + 4 output + 20 read + 0 write.
        assert_eq!(over.total_tokens, Some(24));
    }

    /// Absent fields collapse to zero and optional faces stay absent.
    #[test]
    fn absent_fields_yield_zero_accounting() {
        let empty = usage(json!({}));
        assert_eq!(empty.input_tokens, 0);
        assert_eq!(empty.output_tokens, 0);
        assert_eq!(empty.total_tokens, Some(0));
        // The ??/|| chains bottom out at 0, so every count face dsh fills
        // is present.
        assert_eq!(empty.cache_read_tokens, Some(0));
        assert_eq!(empty.cache_write_tokens, Some(0));
        assert_eq!(empty.reasoning_tokens, Some(0));
    }

    /// The finish-reason mapping: null/stop/end are natural stops, the
    /// legacy tool synonyms map to tool calls, and every provider error
    /// spelling carries the verbatim `Provider finish_reason:` message.
    #[test]
    fn maps_stop_reasons() {
        assert_eq!(
            map_stop_reason(None),
            FinishReason::Known(FinishReasonKind::Stop)
        );
        assert_eq!(
            map_stop_reason(Some("stop")),
            FinishReason::Known(FinishReasonKind::Stop)
        );
        assert_eq!(
            map_stop_reason(Some("end")),
            FinishReason::Known(FinishReasonKind::Stop)
        );
        assert_eq!(
            map_stop_reason(Some("length")),
            FinishReason::Known(FinishReasonKind::MaxTokens)
        );
        assert_eq!(
            map_stop_reason(Some("tool_calls")),
            FinishReason::Known(FinishReasonKind::ToolCalls)
        );
        assert_eq!(
            map_stop_reason(Some("function_call")),
            FinishReason::Known(FinishReasonKind::ToolCalls)
        );
        let FinishReason::Known(FinishReasonKind::Error { failure }) =
            map_stop_reason(Some("content_filter"))
        else {
            panic!("content_filter maps to an error stop");
        };
        assert_eq!(failure.message, "Provider finish_reason: content_filter");
        assert_eq!(failure.code, SERVER_CODE);
        let FinishReason::Known(FinishReasonKind::Error { failure }) =
            map_stop_reason(Some("weird_reason"))
        else {
            panic!("unmapped reasons map to an error stop");
        };
        assert_eq!(failure.message, "Provider finish_reason: weird_reason");
    }
}
