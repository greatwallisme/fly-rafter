// Rust guideline compliant 2026-11-05
//! The pure retry-decision core as a host-free rlib face the plugin
//! shell drives and the tests pin directly.
//!
//! Semantics (dsh `llm-retry/src/index.ts`): `validateConfig`, the
//! policy/state keys, the projection fold, and the `recover` gates. The
//! backoff math itself stays single-sourced in
//! `fr-llm::retry_policy::local_delay`.

use std::collections::HashMap;

use fr_llm::retry_policy::{RetryPolicy, local_delay};

use crate::wire::LlmRetryEventData;

/// The plugin-shell state: the fold table plus the sessions already
/// cold-read (one entry per live session; the read happens on first
/// sight and never repeats within the process).
#[derive(Debug, Default)]
pub struct CoreState {
    /// The rebuilt fold table.
    pub table: Table,
    /// The sessions whose logs were folded already.
    pub folded: std::collections::HashSet<String>,
}

/// One retry chain's folded projection state (dsh `llmRetry` entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The retries already scheduled for the key.
    pub retry: u64,
    /// The chain identity minted with the first retry.
    pub retry_id: String,
}

/// The fold table key: session plus the step-scoped policy identity.
///
/// The turn/step pair in the key IS the dsh fold's step-boundary reset —
/// a lookup can only match entries logged inside the same step, so stale
/// entries from earlier steps are unobservable (and dropped from memory
/// once their session's table is rebuilt from the log).
pub type TableKey = (String, u64, u64, String, String);

/// The in-crate retry state table (the dsh `llmRetry` projection twin;
/// the fr-compaction in-crate session-table judgment).
pub type Table = HashMap<TableKey, Entry>;

/// Validates the executor config (dsh `validateConfig` 1:1): the plugin
/// owns no config; providers own `retryPolicy`.
///
/// The fr dylib form receives no slot config today (the 18a structural
/// note), so this gate is exercised directly by tests and stays live for
/// the config-relay slice; the fr-schema rail rejects unknown keys at
/// assembly time.
///
/// # Errors
///
/// The dsh wording for a first config key: the provider-owned key or an
/// unknown key.
pub fn validate_config(config: &serde_json::Value) -> Result<(), String> {
    let Some(map) = config.as_object() else {
        // The dsh schema layer rejects non-objects before the body runs;
        // the fr rail carries that gate, so this arm is the engine-own
        // fallback text for a direct caller.
        return Err("llm-retry: config must be an object".to_owned());
    };
    // dsh inspects the first key only.
    if let Some((key, _)) = map.iter().next() {
        if key == "retryPolicy" {
            return Err(
                "llm-retry: retryPolicy belongs under each provider configuration".to_owned(),
            );
        }
        return Err(format!("llm-retry: unknown key \"{key}\""));
    }
    Ok(())
}

/// The policy identity key (dsh `retryPolicyKey`).
///
/// The mode's value set as a JSON array, `retryableCodes` sorted so code
/// order never splits a chain.
#[must_use]
pub fn policy_key(policy: &RetryPolicy) -> String {
    match policy {
        RetryPolicy::Normal {
            max_retries,
            retryable_codes,
            backoff,
        } => {
            let mut codes: Vec<&str> = retryable_codes.iter().map(String::as_str).collect();
            codes.sort_unstable();
            serde_json::json!([
                "normal",
                max_retries,
                codes,
                backoff.initial_delay_ms,
                backoff.max_delay_ms,
                backoff.jitter_ratio,
            ])
            .to_string()
        }
        RetryPolicy::Always { backoff } => serde_json::json!([
            "always",
            backoff.initial_delay_ms,
            backoff.max_delay_ms,
            backoff.jitter_ratio,
        ])
        .to_string(),
    }
}

/// The state key (dsh `retryStateKey`): provider plus policy key.
#[must_use]
pub fn state_key(provider: &str, policy_key: &str) -> String {
    serde_json::json!([provider, policy_key]).to_string()
}

/// Rebuilds one session's table from its log events (the cold read fold;
/// the dsh `sessionProjections` fold twin).
///
/// The `llm/retry` data carries every field the fold needs, so a restart
/// rebuilds the exact state; a malformed event is skipped (the fold is
/// advisory scheduling state, never protocol truth).
#[must_use]
pub fn fold(events: &[serde_json::Value], session: &str) -> Table {
    let mut table = Table::new();
    for event in events {
        if event.get("type").and_then(serde_json::Value::as_str)
            != Some(crate::wire::EVENT_LLM_RETRY)
        {
            continue;
        }
        let Some(data) = event.get("data") else {
            continue;
        };
        let Ok(recorded) = serde_json::from_value::<LlmRetryEventData>(data.clone()) else {
            continue;
        };
        table.insert(
            (
                session.to_owned(),
                recorded.turn,
                recorded.step,
                recorded.provider,
                recorded.policy_key,
            ),
            Entry {
                retry: recorded.retry,
                retry_id: recorded.retry_id,
            },
        );
    }
    table
}

/// One scheduled retry: durable `llm/retry` facts plus the loop-side
/// wait.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheduled {
    /// The retry attempt this wait leads into (1-based within the step).
    pub retry: u64,
    /// The chain identity (reused from the entry, or freshly minted).
    pub retry_id: String,
    /// The scheduled wait in milliseconds (the dsh float kept exact).
    pub delay_ms: f64,
}

/// The listener's ruling for one failed attempt (the dsh `recover`
/// outcome).
///
/// Pass the chain through, pull the downstream result first (`always`
/// mode), or schedule one retry.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// The dsh `next()`: no policy, a non-retryable code, or an exhausted
    /// `normal` chain keeps the no-retry default.
    Passthrough,
    /// The dsh `always` arm: settle the downstream chain first, then rule.
    InspectAlways,
    /// Append `llm/retry` and hand the wait to the loop.
    Schedule(Scheduled),
}

/// The wait for one scheduled retry.
///
/// A valid provider `Retry-After` wins up to the ceiling; past it,
/// `normal` passes through (the dsh `next()`) and `always` falls back to
/// the local delay; without a usable hint the local exponential delay
/// applies (dsh `recover`'s delay selection).
fn backoff_delay(
    retry_after_ms: Option<u64>,
    policy: &RetryPolicy,
    retry: u64,
    sample: f64,
) -> Option<f64> {
    let backoff = match policy {
        RetryPolicy::Normal { backoff, .. } | RetryPolicy::Always { backoff } => *backoff,
    };
    match retry_after_ms {
        Some(ms) if ms > 0 => {
            if ms > backoff.max_delay_ms {
                match policy {
                    RetryPolicy::Normal { .. } => None,
                    RetryPolicy::Always { .. } => Some(local_delay(policy, retry, sample)),
                }
            } else {
                Some(f64::from(u32::try_from(ms).unwrap_or(u32::MAX)))
            }
        }
        _ => Some(local_delay(policy, retry, sample)),
    }
}

/// Rules one failed attempt (the dsh `recover` gates before its shared
/// tail).
///
/// `next()` on a missing policy; the `always` arm defers to its
/// downstream first; a non-retryable code or an exhausted `normal` chain
/// passes through; otherwise one retry is scheduled.
pub fn decide(
    policy: Option<&RetryPolicy>,
    failure: &fr_wire::Failure,
    entry: Option<&Entry>,
    new_retry_id: &mut impl FnMut() -> String,
    sample: f64,
) -> Decision {
    let Some(policy) = policy else {
        return Decision::Passthrough;
    };
    match policy {
        RetryPolicy::Always { .. } => Decision::InspectAlways,
        RetryPolicy::Normal {
            max_retries,
            retryable_codes,
            ..
        } => {
            if !retryable_codes.contains(&failure.code) {
                return Decision::Passthrough;
            }
            let previous = entry.map_or(0, |entry| entry.retry);
            if previous >= *max_retries {
                return Decision::Passthrough;
            }
            let retry = previous + 1;
            let retry_id = entry.map_or_else(new_retry_id, |entry| entry.retry_id.clone());
            backoff_delay(failure.provider_retry_after_ms, policy, retry, sample).map_or_else(
                || Decision::Passthrough,
                |delay_ms| {
                    Decision::Schedule(Scheduled {
                        retry,
                        retry_id,
                        delay_ms,
                    })
                },
            )
        }
    }
}

/// Rules the `always` continuation (the dsh `always` arm after
/// `settleDownstream`).
///
/// A downstream retry decision returns verbatim (it carries its own
/// still-pending wait facts); otherwise the unbounded local schedule
/// applies — no code or cap gate on this arm.
#[must_use]
pub fn rule_always(
    downstream_retry: bool,
    policy: &RetryPolicy,
    failure: &fr_wire::Failure,
    entry: Option<&Entry>,
    new_retry_id: &mut impl FnMut() -> String,
    sample: f64,
) -> Option<Scheduled> {
    if downstream_retry {
        return None;
    }
    let retry = entry.map_or(1, |entry| entry.retry + 1);
    let retry_id = entry.map_or_else(new_retry_id, |entry| entry.retry_id.clone());
    let delay_ms = backoff_delay(failure.provider_retry_after_ms, policy, retry, sample);
    Some(Scheduled {
        retry,
        retry_id,
        delay_ms: delay_ms.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::RetryMode;

    fn normal_policy() -> RetryPolicy {
        RetryPolicy::Normal {
            max_retries: 5,
            retryable_codes: vec!["SERVER".to_owned(), "TIMEOUT".to_owned()],
            backoff: fr_llm::retry_policy::Backoff {
                initial_delay_ms: 500,
                max_delay_ms: 10_000,
                jitter_ratio: 0.1,
            },
        }
    }

    fn failure(code: &str, retry_after_ms: Option<u64>) -> fr_wire::Failure {
        fr_wire::Failure {
            message: "boom".to_owned(),
            code: code.to_owned(),
            status: Some(500),
            provider_retry_after_ms: retry_after_ms,
            request_id: None,
            offload_images: None,
        }
    }

    #[test]
    fn config_validation_rejects_keys_with_the_dsh_wording() {
        assert_eq!(validate_config(&serde_json::json!({})), Ok(()));
        assert_eq!(
            validate_config(&serde_json::json!({"retryPolicy": {}})),
            Err("llm-retry: retryPolicy belongs under each provider configuration".to_owned()),
        );
        assert_eq!(
            validate_config(&serde_json::json!({"nope": 1})),
            Err("llm-retry: unknown key \"nope\"".to_owned()),
        );
        assert!(validate_config(&serde_json::json!(3)).is_err());
    }

    #[test]
    fn policy_keys_fold_sorted_codes_and_always_stays_lean() {
        assert_eq!(
            policy_key(&normal_policy()),
            r#"["normal",5,["SERVER","TIMEOUT"],500,10000,0.1]"#,
        );
        assert_eq!(
            policy_key(&RetryPolicy::Always {
                backoff: fr_llm::retry_policy::Backoff {
                    initial_delay_ms: 500,
                    max_delay_ms: 10_000,
                    jitter_ratio: 0.1,
                },
            }),
            r#"["always",500,10000,0.1]"#,
        );
        assert_eq!(
            state_key("deepseek-official", "k"),
            r#"["deepseek-official","k"]"#
        );
    }

    #[test]
    fn fold_rebuilds_entries_and_skips_malformed_events() {
        let good = serde_json::json!({
            "type": crate::wire::EVENT_LLM_RETRY,
            "data": {
                "retryId": "r-1", "turn": 1, "step": 2,
                "provider": "deepseek-official", "mode": "normal",
                "policyKey": "k", "retry": 3, "maxRetries": 5,
                "delayMs": 120.5,
                "failure": {"message": "m", "code": "SERVER"},
            },
        });
        let bad = serde_json::json!({
            "type": crate::wire::EVENT_LLM_RETRY,
            "data": {"retryId": "broken"},
        });
        let table = fold(&[good, bad], "s1");
        let entry = table
            .get(&(
                "s1".to_owned(),
                1,
                2,
                "deepseek-official".to_owned(),
                "k".to_owned(),
            ))
            .expect("the rebuilt entry");
        assert_eq!(entry.retry, 3);
        assert_eq!(entry.retry_id, "r-1");
    }

    #[test]
    fn gates_pass_through_on_missing_policy_codes_and_exhaustion() {
        let mint = &mut || "minted".to_owned();
        assert_eq!(
            decide(None, &failure("SERVER", None), None, mint, 0.0),
            Decision::Passthrough,
        );
        assert_eq!(
            decide(
                Some(&normal_policy()),
                &failure("AUTH", None),
                None,
                mint,
                0.0
            ),
            Decision::Passthrough,
        );
        let spent = Entry {
            retry: 5,
            retry_id: "r-1".to_owned(),
        };
        assert_eq!(
            decide(
                Some(&normal_policy()),
                &failure("SERVER", None),
                Some(&spent),
                mint,
                0.0
            ),
            Decision::Passthrough,
        );
    }

    #[test]
    fn retry_after_wins_up_to_the_ceiling_and_passes_past_it() {
        let mint = &mut || "minted".to_owned();
        let Decision::Schedule(scheduled) = decide(
            Some(&normal_policy()),
            &failure("SERVER", Some(2_000)),
            None,
            mint,
            0.0,
        ) else {
            panic!("a usable hint schedules");
        };
        assert_eq!(scheduled.delay_ms, 2_000.0);
        assert_eq!(scheduled.retry, 1);
        assert_eq!(
            decide(
                Some(&normal_policy()),
                &failure("SERVER", Some(20_000)),
                None,
                mint,
                0.0
            ),
            Decision::Passthrough,
        );
    }

    #[test]
    fn local_backoff_brides_between_the_sample_bounds() {
        let mint = &mut || "minted".to_owned();
        let Decision::Schedule(low) = decide(
            Some(&normal_policy()),
            &failure("SERVER", None),
            None,
            mint,
            0.0,
        ) else {
            panic!("the local arm schedules");
        };
        assert_eq!(low.delay_ms, 450.0);
        let Decision::Schedule(high) = decide(
            Some(&normal_policy()),
            &failure("SERVER", None),
            None,
            mint,
            1.0,
        ) else {
            panic!("the local arm schedules");
        };
        assert_eq!(high.delay_ms, 550.0);
    }

    #[test]
    fn always_ruling_returns_the_downstream_decision_and_never_caps() {
        let policy = RetryPolicy::Always {
            backoff: fr_llm::retry_policy::Backoff {
                initial_delay_ms: 500,
                max_delay_ms: 10_000,
                jitter_ratio: 0.1,
            },
        };
        assert_eq!(
            rule_always(
                true,
                &policy,
                &failure("AUTH", None),
                None,
                &mut || "x".to_owned(),
                0.0
            ),
            None,
        );
        let past_cap = Entry {
            retry: 40,
            retry_id: "r-9".to_owned(),
        };
        let scheduled = rule_always(
            false,
            &policy,
            &failure("AUTH", Some(20_000)),
            Some(&past_cap),
            &mut || "x".to_owned(),
            0.0,
        )
        .expect("always rules past any cap");
        assert_eq!(scheduled.retry, 41);
        assert_eq!(scheduled.retry_id, "r-9");
    }

    #[test]
    fn event_data_pairs_max_retries_with_the_mode() {
        let mint = &mut || "minted".to_owned();
        let Decision::Schedule(scheduled) = decide(
            Some(&normal_policy()),
            &failure("SERVER", None),
            None,
            mint,
            0.0,
        ) else {
            panic!("schedules");
        };
        let build = |mode: RetryMode, max_retries: Option<u64>| LlmRetryEventData {
            retry_id: scheduled.retry_id.clone(),
            turn: 1,
            step: 2,
            provider: "deepseek-official".to_owned(),
            mode,
            policy_key: "k".to_owned(),
            retry: scheduled.retry,
            max_retries,
            delay_ms: scheduled.delay_ms,
            failure: failure("SERVER", None),
        };
        let encoded = serde_json::to_value(build(RetryMode::Normal, Some(5))).expect("encode");
        assert_eq!(encoded["mode"], "normal");
        assert_eq!(encoded["maxRetries"], 5);
        assert!(encoded.get("retryId").is_some());
        let encoded = serde_json::to_value(build(RetryMode::Always, None)).expect("encode");
        assert_eq!(encoded["mode"], "always");
        assert!(encoded.get("maxRetries").is_none());
    }
}
