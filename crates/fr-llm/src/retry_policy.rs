//! Provider retry policy structure, validation, and delay computation (dsh
//! `dsh-llm/retry-policy.ts` + the `llm-retry` executor's `localDelay`).

use crate::error::DEFAULT_RETRYABLE_CODES;

/// Upper bound for every timer delay (dsh `MAX_TIMER_DELAY_MS`, 2^31 - 1).
pub const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

/// Upper bound shared by every dsh safe integer (2^53 - 1).
pub const SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

/// Default maximum retries for the `normal` mode.
pub const DEFAULT_MAX_RETRIES: u64 = 5;
/// Default first-backoff delay in milliseconds.
pub const DEFAULT_INITIAL_DELAY_MS: u64 = 500;
/// Default backoff ceiling in milliseconds.
pub const DEFAULT_MAX_DELAY_MS: u64 = 10_000;
/// Default symmetric jitter ratio.
pub const DEFAULT_JITTER_RATIO: f64 = 0.1;

/// How the executor samples jitter: inclusive zero-to-one (dsh
/// `RetryInternals.random` / `Math.random`).
pub type JitterRandom<'a> = &'a mut dyn FnMut() -> f64;

/// Freezable backoff inputs; `None` fields take the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BackoffInput {
    /// First backoff delay in milliseconds.
    pub initial_delay_ms: Option<u64>,
    /// Backoff ceiling in milliseconds.
    pub max_delay_ms: Option<u64>,
    /// Symmetric jitter ratio in `0.0..=1.0`.
    pub jitter_ratio: Option<f64>,
}

/// Resolved backoff facts: every field defaulted and validated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backoff {
    /// First backoff delay in milliseconds.
    pub initial_delay_ms: u64,
    /// Backoff ceiling in milliseconds.
    pub max_delay_ms: u64,
    /// Symmetric jitter ratio in `0.0..=1.0`.
    pub jitter_ratio: f64,
}

/// Retry-policy configuration input.
///
/// `always` ignores the `normal`-only residual fields (dsh keeps both key
/// sets so a mode switch does not break on leftovers, and rejects unknown
/// keys at the schema layer).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RetryPolicyInput {
    /// Maximum retries for the `normal` mode.
    pub max_retries: Option<u64>,
    /// Retryable codes for the `normal` mode.
    pub retryable_codes: Option<Vec<String>>,
    /// Backoff knobs shared by both modes.
    pub backoff: Option<BackoffInput>,
    /// The mode; `false` = `normal`, `true` = `always`.
    pub always: bool,
}

/// A frozen retry policy: every default filled and every bound validated.
#[derive(Debug, Clone, PartialEq)]
pub enum RetryPolicy {
    /// Bounded retries of the configured retryable codes.
    Normal {
        /// Maximum retry attempts per step.
        max_retries: u64,
        /// Codes worth retrying; anything else fails through.
        retryable_codes: Vec<String>,
        /// Validated backoff facts.
        backoff: Backoff,
    },
    /// Unbounded retries until success, cancellation, or disposal.
    Always {
        /// Validated backoff facts.
        backoff: Backoff,
    },
}

impl RetryPolicy {
    /// Whether `code` is worth retrying under this policy (`always` retries
    /// every failure).
    #[must_use]
    pub fn is_retryable(&self, code: &str) -> bool {
        match self {
            Self::Normal {
                retryable_codes, ..
            } => retryable_codes.iter().any(|candidate| candidate == code),
            Self::Always { .. } => true,
        }
    }

    /// The validated backoff facts of either mode.
    #[must_use]
    pub const fn backoff(&self) -> Backoff {
        match self {
            Self::Normal { backoff, .. } | Self::Always { backoff } => *backoff,
        }
    }
}

/// Validate one backoff input against the shared bounds (dsh
/// `resolveBackoff`): every present field is a positive finite number no
/// greater than [`MAX_TIMER_DELAY_MS`], the initial delay does not exceed
/// the ceiling, and the jitter ratio lies in `0.0..=1.0`.
///
/// `path` is the full diagnostic path of the backoff object (dsh passes
/// `` `${path}.backoff` ``); every rejection message embeds it verbatim
/// before the offending field name (Q60-A, 2026-10-25).
fn resolve_backoff(input: Option<BackoffInput>, path: &str) -> Result<Backoff, String> {
    let input = input.unwrap_or_default();
    let initial_delay_ms = input.initial_delay_ms.unwrap_or(DEFAULT_INITIAL_DELAY_MS);
    let max_delay_ms = input.max_delay_ms.unwrap_or(DEFAULT_MAX_DELAY_MS);
    let jitter_ratio = input.jitter_ratio.unwrap_or(DEFAULT_JITTER_RATIO);
    for (name, value) in [
        ("initialDelayMs", initial_delay_ms),
        ("maxDelayMs", max_delay_ms),
    ] {
        if value == 0 || value > MAX_TIMER_DELAY_MS {
            return Err(format!(
                "{path}.{name} must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
            ));
        }
    }
    if initial_delay_ms > max_delay_ms {
        return Err(format!(
            "{path}.initialDelayMs must be less than or equal to maxDelayMs"
        ));
    }
    // f64 fields: reject non-finite and out-of-range values (dsh validates
    // `positive finite` and `0..1`).
    if !jitter_ratio.is_finite() || !(0.0..=1.0).contains(&jitter_ratio) {
        return Err(format!("{path}.jitterRatio must be between 0 and 1"));
    }
    Ok(Backoff {
        initial_delay_ms,
        max_delay_ms,
        jitter_ratio,
    })
}

/// Resolve, validate, and freeze a retry policy (dsh `resolveRetryPolicy`).
///
/// Validation follows the dsh arm order (Q60-A): the `always` arm validates
/// backoff only; the `normal` arm validates `maxRetries`, then the
/// retryable-code list, then backoff. The dsh `undefined`-config arm has no
/// separate counterpart: an all-default input passes through the `normal`
/// arm with the same outcome.
///
/// # Errors
/// Diagnostic messages verbatim from dsh `retry-policy.ts:128-193`:
/// `{path}.{field} must be ...`, where backoff rejections use the
/// `{path}.backoff.{field}` shape. The dsh `mode` and unknown-key arms are
/// carried by the Rust type system and the provider deserialization layer
/// respectively, so no runtime text exists for them here.
pub fn resolve_retry_policy(input: RetryPolicyInput, path: &str) -> Result<RetryPolicy, String> {
    if input.always {
        let backoff = resolve_backoff(input.backoff, &format!("{path}.backoff"))?;
        return Ok(RetryPolicy::Always { backoff });
    }
    let max_retries = input.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
    if max_retries > SAFE_INTEGER_MAX {
        return Err(format!(
            "{path}.maxRetries must be a non-negative safe integer"
        ));
    }
    let retryable_codes = match input.retryable_codes {
        Some(codes) => {
            if codes.is_empty() {
                return Err(format!("{path}.retryableCodes must not be empty"));
            }
            if codes.iter().any(String::is_empty) {
                return Err(format!(
                    "{path}.retryableCodes must contain only non-empty strings"
                ));
            }
            let mut seen = std::collections::HashSet::new();
            if codes.iter().any(|code| !seen.insert(code.clone())) {
                return Err(format!("{path}.retryableCodes must not contain duplicates"));
            }
            codes
        }
        None => DEFAULT_RETRYABLE_CODES
            .iter()
            .map(|code| (*code).to_owned())
            .collect(),
    };
    let backoff = resolve_backoff(input.backoff, &format!("{path}.backoff"))?;
    Ok(RetryPolicy::Normal {
        max_retries,
        retryable_codes,
        backoff,
    })
}

/// The retry wait for one attempt (dsh `llm-retry` `localDelay` verbatim).
///
/// Exponential growth from the initial delay capped at the ceiling, then
/// symmetric jitter `[1 - ratio, 1 + ratio]`, then the ceiling again.
///
/// `retry` is one-based (the first retry is 1); the exponent saturates at
/// 1024 so far-future attempts never overflow the float. Returns
/// milliseconds; the caller owns the timer.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "dsh computes this delay in float arithmetic; delay values are bounded by maxDelayMs (<= 2^31 - 1), far inside the f64 mantissa"
)]
#[expect(
    clippy::suboptimal_flops,
    reason = "keeps the dsh localDelay expression shape verbatim (jitter = 1 - r + 2r*random)"
)]
pub fn local_delay(policy: &RetryPolicy, retry: u64, random: f64) -> f64 {
    let backoff = policy.backoff();
    let exponent = retry.saturating_sub(1).min(1_024);
    let growth = 2.0_f64.powi(i32::try_from(exponent).unwrap_or(1_024));
    let exponential = (backoff.initial_delay_ms as f64 * growth).min(backoff.max_delay_ms as f64);
    let jitter = 1.0 - backoff.jitter_ratio + 2.0 * backoff.jitter_ratio * random;
    (exponential * jitter).min(backoff.max_delay_ms as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_defaults_and_local_delay() {
        let default_input = RetryPolicyInput {
            max_retries: None,
            retryable_codes: None,
            backoff: None,
            always: false,
        };
        let policy = resolve_retry_policy(default_input.clone(), "llm: provider \"p\" retryPolicy")
            .expect("defaults");
        // Defaults: 5 retries over the five default codes, 500ms/10s/0.1.
        let RetryPolicy::Normal {
            max_retries,
            retryable_codes,
            backoff,
        } = &policy
        else {
            panic!("normal mode expected");
        };
        assert_eq!(*max_retries, 5);
        assert_eq!(
            retryable_codes.as_slice(),
            [
                crate::error::EMPTY_RESPONSE_CODE,
                crate::error::RATE_LIMIT_CODE,
                crate::error::SERVER_CODE,
                crate::error::TIMEOUT_CODE,
                crate::error::TRANSPORT_CODE
            ]
        );
        assert_eq!(backoff.initial_delay_ms, 500);
        assert_eq!(backoff.max_delay_ms, 10_000);
        assert!((backoff.jitter_ratio - 0.1).abs() < f64::EPSILON);
        // localDelay: first retry, zero jitter sample -> initial * (1 - r).
        let first = local_delay(&policy, 1, 0.0);
        assert!((first - 450.0).abs() < f64::EPSILON);
        // Jitter upper sample -> initial * (1 + r).
        let upper = local_delay(&policy, 1, 1.0);
        assert!((upper - 550.0).abs() < f64::EPSILON);
        // Growth saturates at the ceiling regardless of the retry count.
        let saturated = local_delay(&policy, 64, 1.0);
        assert!((saturated - 10_000.0).abs() < f64::EPSILON);
        // `always` mode retries everything and ignores normal-only residuals.
        let always = resolve_retry_policy(
            RetryPolicyInput {
                max_retries: Some(3),
                retryable_codes: Some(vec!["X".to_owned()]),
                backoff: None,
                always: true,
            },
            "llm: provider \"p\" retryPolicy",
        )
        .expect("always");
        assert!(always.is_retryable("ANY_CODE"));
        assert_eq!(always.backoff().initial_delay_ms, 500);
        // Violations carry the dsh path.
        let bad = resolve_retry_policy(
            RetryPolicyInput {
                retryable_codes: Some(Vec::new()),
                ..default_input
            },
            "llm: provider \"p\" retryPolicy",
        );
        assert_eq!(
            bad.expect_err("empty codes"),
            "llm: provider \"p\" retryPolicy.retryableCodes must not be empty"
        );
    }

    // Pins all eight dsh resolveRetryPolicy/resolveBackoff rejection texts
    // verbatim (retry-policy.ts:128-193, Q60-A) plus the normal-arm
    // validation order: maxRetries, then retryableCodes, then backoff.
    #[test]
    fn diagnostics_pin_dsh_texts() {
        let path = "llm: provider \"p\" retryPolicy";
        let reject =
            |input: RetryPolicyInput| resolve_retry_policy(input, path).expect_err("rejection");
        let backoff = |initial: u64, max: u64, jitter: f64| BackoffInput {
            initial_delay_ms: Some(initial),
            max_delay_ms: Some(max),
            jitter_ratio: Some(jitter),
        };
        // The two delay-ceiling texts (dsh :128, :131).
        assert_eq!(
            reject(RetryPolicyInput {
                backoff: Some(backoff(0, 10_000, 0.1)),
                ..RetryPolicyInput::default()
            }),
            format!(
                "{path}.backoff.initialDelayMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
            )
        );
        assert_eq!(
            reject(RetryPolicyInput {
                backoff: Some(backoff(500, MAX_TIMER_DELAY_MS + 1, 0.1)),
                ..RetryPolicyInput::default()
            }),
            format!(
                "{path}.backoff.maxDelayMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
            )
        );
        // Ordering texts (dsh :134, :137).
        assert_eq!(
            reject(RetryPolicyInput {
                backoff: Some(backoff(10_000, 500, 0.1)),
                ..RetryPolicyInput::default()
            }),
            format!("{path}.backoff.initialDelayMs must be less than or equal to maxDelayMs")
        );
        assert_eq!(
            reject(RetryPolicyInput {
                backoff: Some(backoff(500, 10_000, f64::NAN)),
                ..RetryPolicyInput::default()
            }),
            format!("{path}.backoff.jitterRatio must be between 0 and 1")
        );
        // Normal-arm texts (dsh :168, :171, :174, :177).
        assert_eq!(
            reject(RetryPolicyInput {
                max_retries: Some(SAFE_INTEGER_MAX + 1),
                ..RetryPolicyInput::default()
            }),
            format!("{path}.maxRetries must be a non-negative safe integer")
        );
        assert_eq!(
            reject(RetryPolicyInput {
                retryable_codes: Some(vec![String::new()]),
                ..RetryPolicyInput::default()
            }),
            format!("{path}.retryableCodes must contain only non-empty strings")
        );
        assert_eq!(
            reject(RetryPolicyInput {
                retryable_codes: Some(vec!["A".to_owned(), "A".to_owned()]),
                ..RetryPolicyInput::default()
            }),
            format!("{path}.retryableCodes must not contain duplicates")
        );
        // Arm order: an invalid backoff never preempts an earlier normal-arm
        // violation (dsh validates backoff last in the normal arm).
        assert_eq!(
            reject(RetryPolicyInput {
                max_retries: Some(SAFE_INTEGER_MAX + 1),
                backoff: Some(backoff(0, 10_000, 0.1)),
                ..RetryPolicyInput::default()
            }),
            format!("{path}.maxRetries must be a non-negative safe integer")
        );
        assert_eq!(
            reject(RetryPolicyInput {
                retryable_codes: Some(Vec::new()),
                backoff: Some(backoff(0, 10_000, 0.1)),
                ..RetryPolicyInput::default()
            }),
            format!("{path}.retryableCodes must not be empty")
        );
    }
}
