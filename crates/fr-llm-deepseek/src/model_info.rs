//! Protocol-independent model capabilities and reasoning choices (dsh
//! `model-info.ts`).

use fr_llm::adapter::{
    LlmModelInfo, ModelContext, ModelModality, ModelReasoningInfo, ReasoningEffortInfo,
    ResolvedModelInfo,
};
use fr_wire::ids::ReasoningEffortId;

use crate::models::CatalogModel;
use crate::types::ConnectionOptions;

fn effort(id: &'static str, name: &str, description: &str) -> ReasoningEffortInfo {
    ReasoningEffortInfo {
        id: ReasoningEffortId::new(id),
        name: name.to_owned(),
        description: Some(description.to_owned()),
    }
}

/// The four selectable efforts of a thinking-enabled route (dsh
/// `REASONING_EFFORTS`).
fn reasoning_efforts() -> Vec<ReasoningEffortInfo> {
    vec![
        effort(
            "off",
            "Off",
            "Use for simple tasks that do not need reasoning.",
        ),
        effort(
            "low",
            "Low",
            "Prefer for routine or latency-sensitive tasks.",
        ),
        effort("high", "High", "The default balance for most tasks."),
        effort("max", "Max", "Reserve for the hardest quality-first tasks."),
    ]
}

/// The only effort of a thinking-disabled route (dsh
/// `OFF_ONLY_REASONING_EFFORTS`).
fn off_only_efforts() -> Vec<ReasoningEffortInfo> {
    vec![effort(
        "off",
        "Off",
        "Use for simple tasks that do not need reasoning.",
    )]
}

/// Advertise one catalog entry (dsh `catalogModelInfo`).
#[must_use]
pub fn catalog_model_info(provider: &str, model: &CatalogModel) -> LlmModelInfo {
    LlmModelInfo {
        provider: provider.to_owned(),
        id: model.id.clone(),
        name: model.name.clone().unwrap_or_else(|| model.id.clone()),
        description: model.description.clone(),
        input_modalities: Some(
            model
                .input_modalities
                .clone()
                .unwrap_or_else(|| vec![ModelModality::Text]),
        ),
    }
}

/// Resolve model capabilities against one configuration generation (dsh
/// `modelInfo`).
///
/// An uncatalogued endpoint is safely treated as text-only: declaring an
/// unverified image capability would let the host persist input the
/// endpoint may reject on every later turn.
#[must_use]
pub fn model_info(
    connection: &ConnectionOptions,
    provider: &str,
    model: &str,
) -> ResolvedModelInfo {
    let configured = connection.model(model);
    let context_window = configured.map_or(connection.default_context_window, |entry| {
        entry
            .context_window
            .unwrap_or(connection.default_context_window)
    });
    let mut resolved = configured.map_or_else(
        || ResolvedModelInfo {
            provider: provider.to_owned(),
            id: model.to_owned(),
            name: model.to_owned(),
            description: None,
            input_modalities: Some(vec![ModelModality::Text]),
            ..ResolvedModelInfo::default()
        },
        |entry| ResolvedModelInfo {
            provider: provider.to_owned(),
            id: entry.id.clone(),
            name: entry.name.clone().unwrap_or_else(|| entry.id.clone()),
            description: entry.description.clone(),
            input_modalities: Some(
                entry
                    .input_modalities
                    .clone()
                    .unwrap_or_else(|| vec![ModelModality::Text]),
            ),
            ..ResolvedModelInfo::default()
        },
    );
    resolved.context = Some(ModelContext { context_window });
    resolved.default_max_tokens = Some(
        configured
            .and_then(|entry| entry.max_tokens)
            .unwrap_or(connection.max_tokens),
    );
    resolved.system_prompt_update = configured.and_then(|entry| entry.system_prompt_update);
    resolved.tool_update = configured.and_then(|entry| entry.tool_update);
    let default_effort = match connection.defaults.reasoning_effort {
        Some(crate::types::EffortLevel::Off) => ReasoningEffortId::new("off"),
        Some(crate::types::EffortLevel::Low) => ReasoningEffortId::new("low"),
        Some(crate::types::EffortLevel::Max) => ReasoningEffortId::new("max"),
        Some(crate::types::EffortLevel::High) | None => ReasoningEffortId::new("high"),
    };
    resolved.reasoning = Some(
        if connection.defaults.thinking == Some(crate::types::ThinkingMode::Disabled) {
            ModelReasoningInfo {
                efforts: off_only_efforts(),
                default_effort: Some(ReasoningEffortId::new("off")),
            }
        } else {
            ModelReasoningInfo {
                efforts: reasoning_efforts(),
                default_effort: Some(default_effort),
            }
        },
    );
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigOptions;
    use crate::config::resolve_adapter_options;
    use crate::types::{EffortLevel, ThinkingMode};

    fn connection(
        thinking: Option<ThinkingMode>,
        effort: Option<EffortLevel>,
    ) -> crate::types::ConnectionOptions {
        let options = ConfigOptions {
            thinking,
            reasoning_effort: effort,
            ..ConfigOptions::default()
        };
        resolve_adapter_options(&options, None).expect("resolve")
    }

    #[test]
    fn model_info_pins_dsh_metadata() {
        // The catalogued image model: advisory metadata plus the model's
        // own output cap.
        let resolved = model_info(
            &connection(None, None),
            "deepseek-official",
            "deepseek-flash",
        );
        assert_eq!(resolved.id, "deepseek-flash");
        assert_eq!(resolved.name, "DeepSeek-V4.1-Flash");
        assert_eq!(
            resolved.input_modalities.as_deref(),
            Some(&[ModelModality::Text, ModelModality::Image][..])
        );
        assert_eq!(
            resolved.context,
            Some(ModelContext {
                context_window: 1_000_000
            })
        );
        assert_eq!(resolved.default_max_tokens, Some(256_000));
        assert_eq!(
            resolved.system_prompt_update,
            Some(fr_wire::call_config::SystemPromptUpdate::InHistory)
        );
        // Thinking enabled: the four-effort ladder with the configured
        // default; omission defaults to high.
        let reasoning = resolved.reasoning.expect("reasoning");
        assert_eq!(reasoning.efforts.len(), 4);
        assert_eq!(reasoning.efforts[3].id.as_str(), "max");
        assert_eq!(
            reasoning.efforts[3].description.as_deref(),
            Some("Reserve for the hardest quality-first tasks.")
        );
        assert_eq!(
            reasoning.default_effort.map(|id| id.as_str().to_owned()),
            Some("high".to_owned())
        );

        let low = model_info(
            &connection(None, Some(EffortLevel::Low)),
            "p",
            "deepseek-flash",
        );
        assert_eq!(
            low.reasoning
                .expect("reasoning")
                .default_effort
                .map(|id| id.as_str().to_owned()),
            Some("low".to_owned())
        );

        // Thinking disabled: only off is selectable and default.
        let off = model_info(
            &connection(Some(ThinkingMode::Disabled), None),
            "p",
            "deepseek-flash",
        );
        let reasoning = off.reasoning.expect("reasoning");
        assert_eq!(reasoning.efforts.len(), 1);
        assert_eq!(reasoning.efforts[0].id.as_str(), "off");
        assert_eq!(
            reasoning.default_effort.map(|id| id.as_str().to_owned()),
            Some("off".to_owned())
        );
    }

    #[test]
    fn uncatalogued_model_is_text_only() {
        let resolved = model_info(&connection(None, None), "p", "deployment-only-model");
        assert_eq!(resolved.id, "deployment-only-model");
        assert_eq!(resolved.name, "deployment-only-model");
        assert_eq!(
            resolved.input_modalities.as_deref(),
            Some(&[ModelModality::Text][..])
        );
        // The connection-wide fallbacks apply.
        assert_eq!(
            resolved.context,
            Some(ModelContext {
                context_window: 1_000_000
            })
        );
        assert_eq!(resolved.default_max_tokens, Some(256_000));
        assert_eq!(resolved.system_prompt_update, None);
        assert_eq!(resolved.tool_update, None);
    }
}

// Rust guideline compliant 2026-10-26
