//! Default `DeepSeek` model catalog (dsh `models.ts`, entries verbatim).

use fr_llm::adapter::ModelModality;
use fr_wire::call_config::{SystemPromptUpdate, ToolUpdate};

use crate::defaults::DEFAULT_CONTEXT_WINDOW;

/// One catalog image pixel budget: the 512-by-512 `low` preset or an exact
/// total-pixel bound replacing the published token-grid projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagePixelBudget {
    /// The published `low` detail preset (512-by-512 total pixels).
    Low,
    /// An exact total-pixel budget.
    Pixels(u64),
}

/// Advisory official model entry; deployments may replace the catalog
/// (dsh `DeepSeekCatalogModel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    /// Wire model id accepted by the configured endpoint.
    pub id: String,
    /// Selector label; defaults to `id` at resolve time.
    pub name: Option<String>,
    /// Optional selector detail for deployments with similar variants.
    pub description: Option<String>,
    /// Known combined request/response context capacity.
    pub context_window: Option<u64>,
    /// Per-request output cap; omission falls back to the profile's cap.
    pub max_tokens: Option<u64>,
    /// Accepted request modalities; omission is text-only.
    pub input_modalities: Option<Vec<ModelModality>>,
    /// Total-pixel budget for one deterministic request preview.
    pub image_pixel_budget: Option<ImagePixelBudget>,
    /// Encoded-byte target for one deterministic request preview.
    pub image_max_bytes: Option<u64>,
    /// Mid-conversation system prompt handling.
    pub system_prompt_update: Option<SystemPromptUpdate>,
    /// Mid-conversation tool declaration handling.
    pub tool_update: Option<ToolUpdate>,
}

/// Default `DeepSeek` model catalog (dsh `models.ts` `DEFAULT_MODELS`); a
/// function because `String` fields cannot be built in a `const`.
#[must_use]
pub fn default_models() -> [CatalogModel; 2] {
    [
        CatalogModel {
            id: "deepseek-flash".into(),
            name: Some("DeepSeek-V4.1-Flash".into()),
            description: None,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            max_tokens: None,
            input_modalities: Some(vec![ModelModality::Text, ModelModality::Image]),
            image_pixel_budget: None,
            image_max_bytes: None,
            system_prompt_update: Some(SystemPromptUpdate::InHistory),
            tool_update: Some(ToolUpdate::AdditionOnly),
        },
        CatalogModel {
            id: "deepseek-v4-pro".into(),
            name: Some("DeepSeek-V4-Pro".into()),
            description: Some(
                "Stronger agentic coding, knowledge, and difficult reasoning; suited to complex \
             or quality-critical tasks at higher cost."
                    .into(),
            ),
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            max_tokens: None,
            input_modalities: None,
            image_pixel_budget: None,
            image_max_bytes: None,
            system_prompt_update: None,
            tool_update: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_models_pin_dsh_entries() {
        let models = default_models();
        let (flash, pro) = (&models[0], &models[1]);
        assert_eq!(flash.id, "deepseek-flash");
        assert_eq!(flash.name.as_deref(), Some("DeepSeek-V4.1-Flash"));
        assert_eq!(flash.context_window, Some(1_000_000));
        assert_eq!(
            flash.input_modalities.as_deref(),
            Some(&[ModelModality::Text, ModelModality::Image][..])
        );
        assert_eq!(
            flash.system_prompt_update,
            Some(SystemPromptUpdate::InHistory)
        );
        assert_eq!(flash.tool_update, Some(ToolUpdate::AdditionOnly));
        assert_eq!(pro.id, "deepseek-v4-pro");
        assert_eq!(pro.name.as_deref(), Some("DeepSeek-V4-Pro"));
        assert_eq!(pro.context_window, Some(1_000_000));
        assert!(
            pro.description
                .as_deref()
                .unwrap()
                .starts_with("Stronger agentic coding")
        );
        assert_eq!(pro.input_modalities, None);
        assert_eq!(pro.system_prompt_update, None);
        assert_eq!(pro.tool_update, None);
    }
}
