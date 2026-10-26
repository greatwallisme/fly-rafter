//! Materialization of one provider route's hand-declared model catalog.
//!
//! dsh `llm-pi-ai` `catalog.ts` `resolveRouteModels` restricted to the
//! no-catalog face: fr has no installed catalog (PRD 19.2 item 20), so
//! every model is spelled out in configuration and the catalog-default
//! branches are unreachable.
//!
//! Validation keeps the dsh diagnostics verbatim in their English wording;
//! the owning-plugin prefix renders as `fr-llm-openai` and config keys
//! inside the texts render in fr kebab-case (recorded batch adaptation).

use crate::compat::{
    Api, CompatProfile, ResolvedCompat, all_offered_compat_fields, compat_field_protocols,
    configured_compat_fields, offered_compat_fields, resolved_completions_compat,
    resolved_responses_compat,
};
use std::collections::BTreeMap;

/// The wire protocol override a route names, validated against the two
/// served protocols (dsh `PROTOCOLS` table restricted by decision 42 D5).
///
/// # Errors
///
/// Returns the dsh-worded diagnostic (fr prefix) naming the unsupported
/// protocol and the two served ones.
pub fn parse_route_api(provider: &str, api: &str) -> Result<Api, String> {
    match api {
        "openai-completions" => Ok(Api::OpenaiCompletions),
        "openai-responses" => Ok(Api::OpenaiResponses),
        _ => Err(format!(
            "fr-llm-openai: provider \"{provider}\" names api \"{api}\", which this build cannot \
             serve; supported protocols are openai-completions, openai-responses"
        )),
    }
}

/// A request modality a model may accept (dsh `MODALITIES`, catalog.ts:48-54).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    /// Text input.
    Text,
    /// Image input.
    Image,
}

impl Modality {
    /// The config literal (`input:` entry).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }
}

/// A pi-ai thinking level, in escalation order (dsh `THINKING_LEVELS`,
/// catalog.ts:75-86).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThinkingLevel {
    /// No reasoning.
    Off,
    /// Minimal reasoning.
    Minimal,
    /// Low reasoning.
    Low,
    /// Medium reasoning.
    Medium,
    /// High reasoning.
    High,
    /// Extra-high reasoning (needs an explicit wire spelling).
    Xhigh,
    /// Maximum reasoning (needs an explicit wire spelling).
    Max,
}

impl ThinkingLevel {
    /// The config literal (`reasoning-efforts` key and `reasoning:` value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Every level in escalation order.
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];
}

/// One level's pinned dispatch decision (dsh `thinkingLevelMap` entry
/// semantics after `resolveModelReasoning` pinning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LevelMapping {
    /// Send this wire spelling.
    Wire(String),
    /// The level is unsupported (pinned null).
    Unsupported,
    /// Supported, send nothing (only `off` may reach this).
    SendNothing,
}

/// The dispatch map every level is decided explicitly (dsh
/// `ThinkingLevelMap` after pinning).
pub type ThinkingLevelMap = BTreeMap<ThinkingLevel, LevelMapping>;

/// The reasoning-efforts declaration of one model entry (dsh
/// `reasoningEfforts` union: absent inherits, `false` disables, a dict
/// declares the offered levels and their wire spellings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningEfforts {
    /// Declare a non-reasoning model.
    Disable,
    /// Keys are offered levels, values their wire spellings; only `off`
    /// may leave its value empty (supported, send nothing).
    Map(BTreeMap<ThinkingLevel, Option<String>>),
}

/// One configured model entry (dsh `PiAiModelProfile` minus the
/// catalog-default branches).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelProfile {
    /// The model id sent to the provider; required and unique per route.
    pub id: String,
    /// Display name; defaults to the id.
    pub name: Option<String>,
    /// Maximum combined request and response context in tokens.
    pub context_window: Option<u64>,
    /// Maximum output tokens (also this model's per-request default).
    pub max_tokens: Option<u64>,
    /// Request modalities; absent (or empty) states no answer here.
    pub input: Option<Vec<Modality>>,
    /// The reasoning-efforts declaration.
    pub reasoning_efforts: Option<ReasoningEfforts>,
    /// Per-model compat switches, winning over the route's per field.
    pub compat: Option<CompatProfile>,
}

/// One materialized model descriptor (pi-ai `Model` restricted to the
/// hand-declared face; cost is absent by decision 42 D7 — the harness
/// never reads pi-ai cost metadata and fr keeps the zero face).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDescriptor {
    /// The model id sent to the provider.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The wire protocol this model speaks.
    pub api: Api,
    /// The provider route key stamped onto the model.
    pub provider: String,
    /// The endpoint this model targets.
    pub base_url: String,
    /// Request modalities this model accepts.
    pub input: Vec<Modality>,
    /// Maximum combined request and response context in tokens.
    pub context_window: u64,
    /// Maximum output tokens (capability and per-request default).
    pub max_tokens: u64,
    /// Whether the model reasons at all.
    pub reasoning: bool,
    /// The dispatch map; present only on a reasoning model.
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// The resolved compat block for the model's protocol.
    pub compat: ResolvedCompat,
}

/// One route's materialized catalog plus the per-request output caps its
/// profile explicitly configured (dsh `RouteCatalog` minus the
/// deferred-mode error map — fr validates strict only).
#[derive(Debug, Clone, PartialEq)]
pub struct RouteCatalog {
    /// The materialized models in configuration order.
    pub models: Vec<ModelDescriptor>,
    /// Per-request output caps this profile explicitly configured, by id.
    pub configured_max_tokens: BTreeMap<String, u64>,
}

/// The route-level facts model materialization reads (dsh
/// `RouteCatalogRequest` minus the catalog branches).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RouteCatalogRequest {
    /// Provider route key, stamped onto every materialized model.
    pub provider: String,
    /// Wire protocol override; required on this crate (no catalog).
    pub api: Option<Api>,
    /// Endpoint override; required on this crate.
    pub base_url: Option<String>,
    /// The configured models, in configuration order.
    pub models: Vec<ModelProfile>,
    /// The route-level compat switches.
    pub compat: Option<CompatProfile>,
    /// Context capacity default (route `default-context-window`).
    pub default_context_window: u64,
    /// Output capability default (route `default-max-tokens`).
    pub default_max_tokens: u64,
    /// Modality default (route `default-input`).
    pub default_input: Vec<Modality>,
}

/// Reports a route the deployment cannot serve, naming the settings key at
/// fault (dsh `invalid`, catalog.ts:676-678; prefix adapted to the fr
/// artifact name).
fn invalid(provider: &str, detail: &str) -> String {
    format!("fr-llm-openai: provider \"{provider}\" {detail}")
}

/// Resolves one model's reasoning capability from its declared efforts
/// (dsh `resolveModelReasoning`, catalog.ts:719-773): a declared dict
/// pins every level explicitly — declared levels carry their wire
/// spelling, undeclared levels pin to unsupported — while a declared
/// valueless `off` stays absent from the map ("supported, send nothing").
fn resolve_model_reasoning(
    provider: &str,
    entry: &ModelProfile,
) -> Result<(bool, Option<ThinkingLevelMap>), String> {
    let Some(efforts) = &entry.reasoning_efforts else {
        // No catalog exists to inherit from (PRD 19.2 item 20): a bare
        // model without the field does not reason.
        return Ok((false, None));
    };
    if matches!(efforts, ReasoningEfforts::Disable) {
        return Ok((false, None));
    }
    let ReasoningEfforts::Map(efforts) = efforts else {
        return Ok((false, None));
    };
    if efforts.is_empty() {
        return Err(invalid(
            provider,
            &format!(
                "model \"{}\" has an empty reasoning-efforts; declare the offered levels, set \
                 false for a non-reasoning model, or omit the field to keep the installed \
                 catalog's capability",
                entry.id
            ),
        ));
    }
    for (level, wire) in efforts {
        match wire {
            None if matches!(level, ThinkingLevel::Off) => {}
            None => {
                return Err(invalid(
                    provider,
                    &format!(
                        "model \"{}\" reasoning-efforts.{} needs the wire value dispatch should \
                         send; only \"off\" may leave it empty",
                        entry.id,
                        level.as_str()
                    ),
                ));
            }
            Some(wire) if wire.is_empty() => {
                return Err(invalid(
                    provider,
                    &format!(
                        "model \"{}\" reasoning-efforts.{} must not be an empty string",
                        entry.id,
                        level.as_str()
                    ),
                ));
            }
            Some(_) => {}
        }
    }
    if !efforts
        .keys()
        .any(|level| !matches!(level, ThinkingLevel::Off))
    {
        return Err(invalid(
            provider,
            &format!(
                "model \"{}\" reasoning-efforts offers no level beyond \"off\"; declare a \
                 thinking level, or set reasoning-efforts to false for a non-reasoning model",
                entry.id
            ),
        ));
    }
    let mut map = BTreeMap::new();
    for level in ThinkingLevel::ALL {
        match efforts.get(&level) {
            None => {
                map.insert(level, LevelMapping::Unsupported);
            }
            Some(None) => {
                map.insert(level, LevelMapping::SendNothing);
            }
            Some(Some(wire)) => {
                map.insert(level, LevelMapping::Wire(wire.clone()));
            }
        }
    }
    Ok((true, Some(map)))
}

/// Resolves one model's compat block (dsh `resolveModelCompat`,
/// catalog.ts:796-827): a model switch wins over the route switch field
/// by field, and a model-level switch its protocol does not take fails
/// resolution while a route-level one is skipped for such models.
fn resolve_model_compat(
    provider: &str,
    entry: &ModelProfile,
    base_url: &str,
    route: Option<&CompatProfile>,
    api: Api,
) -> Result<ResolvedCompat, String> {
    if let Some(model) = &entry.compat {
        for field in configured_compat_fields(model) {
            if !compat_field_protocols(field).contains(&api.as_str()) {
                let offered = offered_compat_fields(api);
                return Err(invalid(
                    provider,
                    &format!(
                        "model \"{}\" sets compat \"{}\", but its api is \"{}\", which does not \
                         take it; that switch exists on {}, and \"{}\" offers {}",
                        entry.id,
                        field,
                        api.as_str(),
                        compat_field_protocols(field).join(", "),
                        api.as_str(),
                        if offered.is_empty() {
                            "no configurable compat".to_string()
                        } else {
                            offered.join(", ")
                        }
                    ),
                ));
            }
        }
    }
    Ok(match api {
        Api::OpenaiCompletions => ResolvedCompat::Completions(resolved_completions_compat(
            provider,
            base_url,
            &entry.id,
            route,
            entry.compat.as_ref(),
        )),
        Api::OpenaiResponses => ResolvedCompat::Responses(resolved_responses_compat(
            provider,
            base_url,
            route,
            entry.compat.as_ref(),
        )),
    })
}

/// Materializes one route's hand-declared catalog (dsh `resolveRouteModels`).
///
/// catalog.ts:857-986, no-catalog face: every model is spelled out in
/// configuration, `model-overrides` is refused outright, and strict
/// validation rejects the first unserviceable fact.
///
/// # Errors
///
/// Returns the dsh-worded diagnostic (fr prefix) for the first failing
/// fact: empty id, duplicate id, missing api or base-url, non-positive
/// capacities, malformed reasoning-efforts, or a compat switch its
/// protocol does not take.
pub fn resolve_route_models(request: &RouteCatalogRequest) -> Result<RouteCatalog, String> {
    let provider = request.provider.as_str();
    if request.models.is_empty() {
        return Err(invalid(
            provider,
            "resolves no models; the installed catalog does not describe this route, so its \
             models must be listed in configuration",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut configured_max_tokens = BTreeMap::new();
    let mut models = Vec::with_capacity(request.models.len());
    for entry in &request.models {
        let model = materialize_model(
            provider,
            request,
            entry,
            &mut seen,
            &mut configured_max_tokens,
        )?;
        models.push(model);
    }
    let api = models
        .first()
        .map_or(Api::OpenaiCompletions, |model: &ModelDescriptor| model.api);
    check_route_compat_takers(provider, api, request.compat.as_ref())?;
    Ok(RouteCatalog {
        models,
        configured_max_tokens,
    })
}

/// Materializes one configured model entry into its descriptor, recording
/// an explicitly configured output cap (the per-model loop of dsh
/// `resolveRouteModels`).
///
/// # Errors
///
/// Returns the dsh-worded diagnostic (fr prefix) for the first failing
/// fact on the entry.
fn materialize_model(
    provider: &str,
    request: &RouteCatalogRequest,
    entry: &ModelProfile,
    seen: &mut std::collections::BTreeSet<String>,
    configured_max_tokens: &mut BTreeMap<String, u64>,
) -> Result<ModelDescriptor, String> {
    if entry.id.is_empty() {
        return Err(invalid(provider, "has a model with an empty id"));
    }
    if !seen.insert(entry.id.clone()) {
        return Err(invalid(
            provider,
            &format!("lists model \"{}\" more than once", entry.id),
        ));
    }
    let api = request.api.ok_or_else(|| {
        invalid(
            provider,
            &format!(
                "model \"{}\" needs an api; the installed catalog does not describe it, so \
                 set the route's api to the wire protocol its endpoint speaks",
                entry.id
            ),
        )
    })?;
    let base_url = request.base_url.clone().ok_or_else(|| {
        invalid(
            provider,
            &format!(
                "model \"{}\" needs a base-url; the installed catalog does not describe this \
                 route",
                entry.id
            ),
        )
    })?;
    let context_window = entry
        .context_window
        .unwrap_or(request.default_context_window);
    if context_window == 0 {
        return Err(invalid(
            provider,
            &format!(
                "model \"{}\" context-window must be a positive integer",
                entry.id
            ),
        ));
    }
    let max_tokens = entry.max_tokens.unwrap_or(request.default_max_tokens);
    if max_tokens == 0 {
        return Err(invalid(
            provider,
            &format!(
                "model \"{}\" max-tokens must be a positive integer",
                entry.id
            ),
        ));
    }
    if let Some(cap) = entry.max_tokens {
        configured_max_tokens.insert(entry.id.clone(), cap);
    }
    let input = match &entry.input {
        Some(input) if !input.is_empty() => input.clone(),
        _ => request.default_input.clone(),
    };
    let (reasoning, thinking_level_map) = resolve_model_reasoning(provider, entry)?;
    let compat = resolve_model_compat(provider, entry, &base_url, request.compat.as_ref(), api)?;
    Ok(ModelDescriptor {
        id: entry.id.clone(),
        name: entry.name.clone().unwrap_or_else(|| entry.id.clone()),
        api,
        provider: provider.to_string(),
        base_url,
        input,
        context_window,
        max_tokens,
        reasoning,
        thinking_level_map,
        compat,
    })
}

/// Refuses a route-level compat switch no model's protocol takes (the
/// route-tail gate of dsh `resolveRouteModels`).
///
/// # Errors
///
/// Returns the dsh-worded diagnostic (fr prefix) naming the fields that
/// were available instead of the field that missed.
fn check_route_compat_takers(
    provider: &str,
    api: Api,
    profile: Option<&CompatProfile>,
) -> Result<(), String> {
    for field in profile.map(configured_compat_fields).unwrap_or_default() {
        let takers = compat_field_protocols(field);
        if takers.contains(&api.as_str()) {
            continue;
        }
        return Err(invalid(
            provider,
            &format!(
                "sets compat \"{field}\", but no model on the route speaks a protocol that takes \
                 it; it exists on {}",
                if takers.is_empty() {
                    all_offered_compat_fields().join(", ")
                } else {
                    takers.join(", ")
                }
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::{MaxTokensField, ThinkingFormat};

    fn request(models: Vec<ModelProfile>) -> RouteCatalogRequest {
        RouteCatalogRequest {
            provider: "acme".to_string(),
            api: Some(Api::OpenaiCompletions),
            base_url: Some("https://gw.example/v1".to_string()),
            models,
            compat: None,
            default_context_window: 262_144,
            default_max_tokens: 32_768,
            default_input: vec![Modality::Text],
        }
    }

    fn model(id: &str) -> ModelProfile {
        ModelProfile {
            id: id.to_string(),
            ..ModelProfile::default()
        }
    }

    #[test]
    fn materializes_a_hand_declared_route() {
        let mut think = model("acme-think");
        think.max_tokens = Some(8_192);
        let catalog =
            resolve_route_models(&request(vec![model("acme-large"), think])).expect("materializes");
        assert_eq!(catalog.models.len(), 2);
        let first = &catalog.models[0];
        assert_eq!(first.name, "acme-large");
        assert_eq!(first.context_window, 262_144);
        assert_eq!(first.max_tokens, 32_768);
        assert_eq!(first.input, vec![Modality::Text]);
        assert!(!first.reasoning);
        assert_eq!(catalog.configured_max_tokens.len(), 1);
        assert_eq!(catalog.configured_max_tokens["acme-think"], 8_192);
        let ResolvedCompat::Completions(compat) = &first.compat else {
            panic!("completions compat");
        };
        assert_eq!(compat.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        assert_eq!(compat.thinking_format, ThinkingFormat::Openai);
    }

    #[test]
    fn missing_api_or_base_url_names_the_first_model() {
        let mut req = request(vec![model("acme-large")]);
        req.api = None;
        let err = resolve_route_models(&req).expect_err("no api");
        assert!(err.contains(
            "model \"acme-large\" needs an api; the installed catalog does not describe it"
        ));
        let mut req = request(vec![model("acme-large")]);
        req.base_url = None;
        let err = resolve_route_models(&req).expect_err("no base url");
        assert!(err.contains(
            "model \"acme-large\" needs a base-url; the installed catalog does not describe \
                 this route"
        ));
    }

    #[test]
    fn duplicate_and_empty_ids_are_refused() {
        let err =
            resolve_route_models(&request(vec![model("a"), model("a")])).expect_err("duplicate");
        assert!(err.contains("lists model \"a\" more than once"));
        let err = resolve_route_models(&request(vec![model("")])).expect_err("empty");
        assert!(err.contains("has a model with an empty id"));
    }

    #[test]
    fn unsupported_api_names_the_served_protocols() {
        let err = parse_route_api("acme", "azure-openai-responses").expect_err("unsupported");
        assert!(err.contains(
            "provider \"acme\" names api \"azure-openai-responses\", which this build cannot \
                 serve; supported protocols are openai-completions, openai-responses"
        ));
    }

    #[test]
    fn reasoning_efforts_pin_every_level() {
        let mut think = model("acme-think");
        think.reasoning_efforts = Some(ReasoningEfforts::Map(BTreeMap::from([
            (ThinkingLevel::Off, None),
            (ThinkingLevel::High, Some("high".to_string())),
        ])));
        let catalog = resolve_route_models(&request(vec![think])).expect("materializes");
        let m = &catalog.models[0];
        assert!(m.reasoning);
        let map = m.thinking_level_map.as_ref().expect("map");
        assert_eq!(map[&ThinkingLevel::Off], LevelMapping::SendNothing);
        assert_eq!(map[&ThinkingLevel::Minimal], LevelMapping::Unsupported);
        assert_eq!(
            map[&ThinkingLevel::High],
            LevelMapping::Wire("high".to_string())
        );
        assert_eq!(map[&ThinkingLevel::Max], LevelMapping::Unsupported);
    }

    #[test]
    fn malformed_reasoning_efforts_are_refused() {
        let mut think = model("acme-think");
        think.reasoning_efforts = Some(ReasoningEfforts::Map(BTreeMap::new()));
        let err = resolve_route_models(&request(vec![think])).expect_err("empty dict");
        assert!(err.contains("has an empty reasoning-efforts"));
        let mut think = model("acme-think");
        think.reasoning_efforts = Some(ReasoningEfforts::Map(BTreeMap::from([
            (ThinkingLevel::Off, None),
            (ThinkingLevel::Xhigh, None),
        ])));
        let err = resolve_route_models(&request(vec![think])).expect_err("null xhigh");
        assert!(err.contains("reasoning-efforts.xhigh needs the wire value dispatch should send"));
        let mut think = model("acme-think");
        think.reasoning_efforts = Some(ReasoningEfforts::Map(BTreeMap::from([
            (ThinkingLevel::Off, None),
            (ThinkingLevel::High, Some(String::new())),
        ])));
        let err = resolve_route_models(&request(vec![think])).expect_err("empty wire");
        assert!(err.contains("reasoning-efforts.high must not be an empty string"));
    }

    #[test]
    fn model_compat_switch_not_taken_by_api_is_refused() {
        let mut entry = model("acme-r");
        entry.compat = Some(CompatProfile {
            thinking_format: Some(ThinkingFormat::Deepseek),
            ..CompatProfile::default()
        });
        let mut req = request(vec![entry]);
        req.api = Some(Api::OpenaiResponses);
        let err = resolve_route_models(&req).expect_err("not taken");
        assert!(err.contains(
            "model \"acme-r\" sets compat \"thinking-format\", but its api is \
                 \"openai-responses\", which does not take it; that switch exists on \
                 openai-completions, and \"openai-responses\" offers supports-developer-role, \
                 supports-max-output-tokens, supports-strict-mode, supports-long-cache-retention"
        ));
    }

    #[test]
    fn route_compat_without_taker_is_refused() {
        let req = RouteCatalogRequest {
            compat: Some(CompatProfile {
                supports_max_output_tokens: Some(true),
                ..CompatProfile::default()
            }),
            ..request(vec![model("acme-large")])
        };
        let err = resolve_route_models(&req).expect_err("no taker");
        assert!(err.contains(
            "sets compat \"supports-max-output-tokens\", but no model on the route speaks a \
                 protocol that takes it; it exists on openai-responses"
        ));
    }

    #[test]
    fn zero_capacities_are_refused() {
        let mut entry = model("acme-large");
        entry.context_window = Some(0);
        let err = resolve_route_models(&request(vec![entry])).expect_err("zero context");
        assert!(err.contains("context-window must be a positive integer"));
        let mut entry = model("acme-large");
        entry.max_tokens = Some(0);
        let err = resolve_route_models(&request(vec![entry])).expect_err("zero max");
        assert!(err.contains("max-tokens must be a positive integer"));
    }
}
// Rust guideline compliant 2026-10-26
