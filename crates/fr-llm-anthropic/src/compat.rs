//! The anthropic-messages compat gate (dsh `llm-pi-ai` `catalog.ts`
//! `ANTHROPIC_COMPAT_GATE`, catalog.ts:292-307).
//!
//! Seven offered switches a hand-declared route or model entry may set;
//! six withheld names the installed catalog fixes (PRD 19.2 item 20: fr
//! ships no catalog, so a withheld name is refused wherever written —
//! decision 45 A2 keeps the in-history and affinity faces unreachable).
//! The vocabulary check runs before any member parse (dsh
//! `assertOfferedCompatFields` semantics): a withheld name is refused
//! wherever written, then a valueless key, then the typed members. The
//! diagnostics keep the dsh wording with the `fr-llm-anthropic` prefix
//! and kebab-case config keys.

/// The wire protocol this crate serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// The Anthropic Messages beta API (`POST /v1/messages?beta=true`).
    AnthropicMessages,
}

impl Api {
    /// The config literal (route `api:` value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

/// The compat fields the configuration surface offers for
/// anthropic-messages (the offer arm of `ANTHROPIC_COMPAT_GATE`,
/// catalog.ts:293-300, rendered in fr kebab-case).
pub(crate) const OFFERED_COMPAT_FIELDS: &[&str] = &[
    "supports-eager-tool-input-streaming",
    "supports-long-cache-retention",
    "supports-cache-control-on-tools",
    "supports-temperature",
    "force-adaptive-thinking",
    "allow-empty-signature",
    "supports-strict-tools",
];

/// The compat names the installed catalog fixes (the withhold arm of
/// `ANTHROPIC_COMPAT_GATE`, catalog.ts:301-306).
const WITHHELD_COMPAT_FIELDS: &[&str] = &[
    "send-session-affinity-headers",
    "session-affinity-format",
    "supports-mid-convo-system-messages",
    "supports-mid-convo-tool-changes",
    "supports-mid-convo-effort",
    "allowed-fallback-models",
];

/// The configured compat switches of one route or model entry (every
/// offered field optional; absent leaves the field to its default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompatProfile {
    /// Per-tool `eager_input_streaming`.
    pub supports_eager_tool_input_streaming: Option<bool>,
    /// The `ttl: "1h"` cache-control form.
    pub supports_long_cache_retention: Option<bool>,
    /// `cache_control` on tool definitions.
    pub supports_cache_control_on_tools: Option<bool>,
    /// The `temperature` request field.
    pub supports_temperature: Option<bool>,
    /// Adaptive thinking regardless of model id.
    pub force_adaptive_thinking: Option<bool>,
    /// Replay an empty thinking signature instead of converting the
    /// thinking block to text.
    pub allow_empty_signature: Option<bool>,
    /// Anthropic strict tool schemas.
    pub supports_strict_tools: Option<bool>,
}

/// The resolved compat block of one materialized model (pi-ai
/// `getAnthropicCompat` defaults, anthropic-messages.ts:215-229).
///
/// The openrouter-derived affinity defaults are withheld faces that keep
/// their non-openrouter values here.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per ANTHROPIC_COMPAT_GATE offer arm (catalog.ts:293-300); the dsh \
              shape is a flat bool record"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnthropicCompat {
    /// Per-tool `eager_input_streaming` (default true).
    pub supports_eager_tool_input_streaming: bool,
    /// The `ttl: "1h"` form (default true).
    pub supports_long_cache_retention: bool,
    /// `cache_control` on tools (default true).
    pub supports_cache_control_on_tools: bool,
    /// The `temperature` field (default true).
    pub supports_temperature: bool,
    /// Adaptive thinking (default false).
    pub force_adaptive_thinking: bool,
    /// Empty thinking signatures (default false).
    pub allow_empty_signature: bool,
    /// Strict tool schemas (default false).
    pub supports_strict_tools: bool,
}

/// Reports a compat diagnostic with the fr artifact prefix (the shared
/// `invalid` shape).
fn invalid_compat(provider: &str, site: &str, detail: &str) -> String {
    format!("fr-llm-anthropic: provider \"{provider}\" {site} {detail}")
}

/// Parses one boolean compat member.
fn parse_compat_bool(field: &str, value: &serde_json::Value) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("compat \"{field}\" must be a boolean"))
}

/// Parses one compat profile (route-level or model-level) from raw
/// configuration (dsh `compatProfile` schema + the
/// `assertOfferedCompatFields` vocabulary gate).
///
/// # Errors
///
/// Names the offending key: withheld (`not configurable here`),
/// undeclared (`which no wire protocol declares`, with the offered
/// switches listed), valueless, or a member with a malformed value.
pub(crate) fn parse_compat_profile(
    provider: &str,
    site: &str,
    value: &serde_json::Value,
) -> Result<CompatProfile, String> {
    let serde_json::Value::Object(map) = value else {
        return Err(invalid_compat(provider, site, "must be an object"));
    };
    let mut profile = CompatProfile::default();
    for (key, member) in map {
        if !OFFERED_COMPAT_FIELDS.contains(&key.as_str()) {
            if WITHHELD_COMPAT_FIELDS.contains(&key.as_str()) {
                return Err(invalid_compat(
                    provider,
                    site,
                    &format!(
                        "sets compat \"{key}\", which is not configurable here: the served \
                         protocols fix this switch, and fr ships no built-in catalog to source \
                         it from"
                    ),
                ));
            }
            return Err(invalid_compat(
                provider,
                site,
                &format!(
                    "sets compat \"{key}\", which no wire protocol declares; the configurable \
                     switches are {}",
                    OFFERED_COMPAT_FIELDS.join(", ")
                ),
            ));
        }
        if member.is_null() {
            return Err(invalid_compat(
                provider,
                site,
                &format!(
                    "sets compat \"{key}\" with no value; give it one, or remove the key to \
                     leave the field to detection"
                ),
            ));
        }
        match key.as_str() {
            "supports-eager-tool-input-streaming" => {
                profile.supports_eager_tool_input_streaming = Some(parse_compat_bool(key, member)?);
            }
            "supports-long-cache-retention" => {
                profile.supports_long_cache_retention = Some(parse_compat_bool(key, member)?);
            }
            "supports-cache-control-on-tools" => {
                profile.supports_cache_control_on_tools = Some(parse_compat_bool(key, member)?);
            }
            "supports-temperature" => {
                profile.supports_temperature = Some(parse_compat_bool(key, member)?);
            }
            "force-adaptive-thinking" => {
                profile.force_adaptive_thinking = Some(parse_compat_bool(key, member)?);
            }
            "allow-empty-signature" => {
                profile.allow_empty_signature = Some(parse_compat_bool(key, member)?);
            }
            "supports-strict-tools" => {
                profile.supports_strict_tools = Some(parse_compat_bool(key, member)?);
            }
            // The vocabulary gate above admits only offered keys, and
            // every offered key has an arm, so this arm fails closed on a
            // build defect instead of panicking.
            other => {
                return Err(invalid_compat(
                    provider,
                    site,
                    &format!("compat \"{other}\" is offered but has no parse arm; build defect"),
                ));
            }
        }
    }
    Ok(profile)
}

/// One configured field's resolution: a model switch wins over the route
/// switch, and an absent field keeps the pi-ai default.
fn pick(route: Option<bool>, model: Option<bool>, default: bool) -> bool {
    model.or(route).unwrap_or(default)
}

/// Resolves one model's compat block (dsh `resolveModelCompat`
/// model-wins rule, catalog.ts:796-827).
///
/// The cross-protocol refusal arms are unreachable on this
/// single-protocol crate — every offered switch is an
/// anthropic-messages switch.
#[must_use]
pub fn resolve_anthropic_compat(
    route: Option<&CompatProfile>,
    model: Option<&CompatProfile>,
) -> AnthropicCompat {
    AnthropicCompat {
        supports_eager_tool_input_streaming: pick(
            route.and_then(|c| c.supports_eager_tool_input_streaming),
            model.and_then(|c| c.supports_eager_tool_input_streaming),
            true,
        ),
        supports_long_cache_retention: pick(
            route.and_then(|c| c.supports_long_cache_retention),
            model.and_then(|c| c.supports_long_cache_retention),
            true,
        ),
        supports_cache_control_on_tools: pick(
            route.and_then(|c| c.supports_cache_control_on_tools),
            model.and_then(|c| c.supports_cache_control_on_tools),
            true,
        ),
        supports_temperature: pick(
            route.and_then(|c| c.supports_temperature),
            model.and_then(|c| c.supports_temperature),
            true,
        ),
        force_adaptive_thinking: pick(
            route.and_then(|c| c.force_adaptive_thinking),
            model.and_then(|c| c.force_adaptive_thinking),
            false,
        ),
        allow_empty_signature: pick(
            route.and_then(|c| c.allow_empty_signature),
            model.and_then(|c| c.allow_empty_signature),
            false,
        ),
        supports_strict_tools: pick(
            route.and_then(|c| c.supports_strict_tools),
            model.and_then(|c| c.supports_strict_tools),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: &serde_json::Value) -> Result<CompatProfile, String> {
        parse_compat_profile("anthropic", "route", value)
    }

    #[test]
    fn withheld_names_are_refused_wherever_written() {
        for key in WITHHELD_COMPAT_FIELDS {
            let err = parse(&json!({ *key: true })).expect_err("withheld");
            assert!(
                err.contains(&format!(
                    "sets compat \"{key}\", which is not configurable here: the served \
                     protocols fix this switch, and fr ships no built-in catalog to source it \
                     from"
                )),
                "got: {err}"
            );
        }
    }

    #[test]
    fn undeclared_names_list_the_offered_switches() {
        let err = parse(&json!({ "supports-nonsense": true })).expect_err("undeclared");
        assert!(
            err.contains(
                "sets compat \"supports-nonsense\", which no wire protocol declares; the \
                 configurable switches are supports-eager-tool-input-streaming, \
                 supports-long-cache-retention, supports-cache-control-on-tools, \
                 supports-temperature, force-adaptive-thinking, allow-empty-signature, \
                 supports-strict-tools"
            ),
            "got: {err}"
        );
    }

    #[test]
    fn valueless_and_malformed_members_are_refused() {
        let err = parse(&json!({ "supports-temperature": null })).expect_err("valueless");
        assert!(
            err.contains(
                "sets compat \"supports-temperature\" with no value; give it one, or remove \
                 the key to leave the field to detection"
            ),
            "got: {err}"
        );
        let err = parse(&json!({ "force-adaptive-thinking": "yes" })).expect_err("not bool");
        assert!(
            err.contains("compat \"force-adaptive-thinking\" must be a boolean"),
            "got: {err}"
        );
        let err = parse(&json!("nope")).expect_err("not object");
        assert!(err.contains("must be an object"), "got: {err}");
    }

    #[test]
    fn model_wins_over_route_over_default() {
        let route = CompatProfile {
            supports_temperature: Some(false),
            supports_strict_tools: Some(true),
            ..CompatProfile::default()
        };
        let model = CompatProfile {
            supports_temperature: Some(true),
            ..CompatProfile::default()
        };
        let resolved = resolve_anthropic_compat(Some(&route), Some(&model));
        assert!(resolved.supports_temperature, "model wins");
        assert!(resolved.supports_strict_tools, "route fills");
        assert!(resolved.supports_eager_tool_input_streaming, "default");
        assert!(!resolved.force_adaptive_thinking, "default false");
        let bare = resolve_anthropic_compat(None, None);
        assert_eq!(
            bare,
            AnthropicCompat {
                supports_eager_tool_input_streaming: true,
                supports_long_cache_retention: true,
                supports_cache_control_on_tools: true,
                supports_temperature: true,
                force_adaptive_thinking: false,
                allow_empty_signature: false,
                supports_strict_tools: false,
            }
        );
    }
}
// Rust guideline compliant 2026-10-27
