//! Wire-compatibility resolution for the two OpenAI-family protocols.
//!
//! Parity with dsh `llm-pi-ai` + pi-ai `openai-completions.ts`/`openai-responses.ts`:
//! a per-route/per-model [`CompatProfile`] carries the switches a
//! deployment states because nothing can interrogate a gateway for them
//! (dsh `config.ts:97-127`); detection from the provider id and base URL
//! (pi-ai `detectCompat`, `openai-completions.ts:1593-1690`) fills every
//! undecided field, and explicit profile entries win field by field
//! (`getCompat` merge, `openai-completions.ts:1696-1734`). Fields dsh
//! withholds from its configuration surface stay unreachable here, which
//! is exactly the dsh behavior for hand-declared routes (Q74-B, PRD
//! decision 42): `supports_mid_convo_*` resolve to `false` on both
//! protocols, so tool-update capability is absent and the baseline folding
//! semantics carry mid-conversation changes.
//!
//! Diagnostic-prefix adaptation (recorded at slice close): dsh texts open
//! with `llm-pi-ai:`; this crate opens with `fr-llm-openai:`, and config
//! keys inside the texts render in the fr kebab-case spellings.

/// The two wire protocols this artifact serves (PRD decision 42 D5: azure
/// and codex responses variants are not served — dsh `provider.ts:47-51`
/// PROTOCOLS table has no hand-declared route for them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// The Chat Completions wire protocol (`/chat/completions`).
    OpenaiCompletions,
    /// The Responses wire protocol (`/responses`).
    OpenaiResponses,
}

impl Api {
    /// The configuration spelling of the protocol (`api:` route value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompletions => "openai-completions",
            Self::OpenaiResponses => "openai-responses",
        }
    }
}

/// The output-cap request field an endpoint reads (pi-ai
/// `OpenAICompletionsCompat['maxTokensField']`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxTokensField {
    /// The modern `OpenAI` spelling.
    MaxCompletionTokens,
    /// The deprecated spelling some OpenAI-compatible providers require.
    MaxTokens,
}

impl MaxTokensField {
    /// The wire/config literal (dsh `MAX_TOKENS_FIELDS` gate, catalog.ts:121-127).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxCompletionTokens => "max_completion_tokens",
            Self::MaxTokens => "max_tokens",
        }
    }
}

/// The reasoning-dispatch dialect an endpoint expects (pi-ai
/// `thinkingFormat`; dsh `SUPPORTED_THINKING_FORMATS` gate, catalog.ts:100-115).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingFormat {
    /// Plain `reasoning_effort` (the `OpenAI` spelling).
    Openai,
    /// `thinking: {type: enabled|disabled}` plus `reasoning_effort`.
    Deepseek,
    /// Nested `reasoning: {effort}` (`OpenRouter` normalization).
    Openrouter,
    /// `reasoning: {enabled}` plus `reasoning_effort`.
    Together,
    /// `chat_template_args` plus `reasoning_effort`.
    Baseten,
    /// `thinking: {type, clear_thinking}` plus `reasoning_effort`.
    Zai,
    /// `enable_thinking` plus `reasoning_effort`.
    Qwen,
    /// `chat_template_kwargs` from the profile's kwargs.
    ChatTemplate,
    /// `chat_template_kwargs` with a fixed enable/preserve pair.
    QwenChatTemplate,
    /// `thinking` as a plain string.
    StringThinking,
    /// Nested `reasoning: {effort}`, only while an effort is selected.
    AntLing,
}

impl ThinkingFormat {
    /// The config literal (`thinking-format:` value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Deepseek => "deepseek",
            Self::Openrouter => "openrouter",
            Self::Together => "together",
            Self::Baseten => "baseten",
            Self::Zai => "zai",
            Self::Qwen => "qwen",
            Self::ChatTemplate => "chat-template",
            Self::QwenChatTemplate => "qwen-chat-template",
            Self::StringThinking => "string-thinking",
            Self::AntLing => "ant-ling",
        }
    }
}

/// The request field carrying the reasoning token budget (pi-ai
/// `thinkingTokenBudgetField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingTokenBudgetField {
    /// The generic spelling.
    ThinkingTokenBudget,
    /// The vLLM spelling.
    ThinkingBudget,
    /// The `SGLang` spelling.
    ThinkingBudgetTokens,
}

impl ThinkingTokenBudgetField {
    /// The wire/config literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThinkingTokenBudget => "thinking_token_budget",
            Self::ThinkingBudget => "thinking_budget",
            Self::ThinkingBudgetTokens => "thinking_budget_tokens",
        }
    }
}

/// The prompt-cache marker convention (pi-ai `cacheControlFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheControlFormat {
    /// Anthropic `cache_control` markers on an OpenAI-shaped body.
    Anthropic,
}

/// The session-affinity header convention (pi-ai `sessionAffinityFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAffinityFormat {
    /// `session_id` + `x-client-request-id` (+ `x-session-affinity` on
    /// completions).
    Openai,
    /// `x-session-id` only.
    Openrouter,
}

/// The `$var` dispatch key a chat-template value may name (dsh
/// `CHAT_TEMPLATE_VAR_GATE`, catalog.ts:157-164).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatTemplateVar {
    /// Dispatch fills a boolean: is reasoning on for this request.
    ThinkingEnabled,
    /// Dispatch fills the selected effort spelling.
    ThinkingEffort,
    /// Dispatch fills the clamped reasoning token budget.
    ThinkingBudget,
}

impl ChatTemplateVar {
    /// The config literal (`$var` value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThinkingEnabled => "thinking.enabled",
            Self::ThinkingEffort => "thinking.effort",
            Self::ThinkingBudget => "thinking.budget",
        }
    }
}

/// A scalar chat-template value (dsh `chatTemplateKwarg` union members
/// that are not `$var` dispatches).
#[derive(Debug, Clone, PartialEq)]
pub enum ScalarValue {
    /// A JSON string.
    Str(String),
    /// A JSON number.
    Num(f64),
    /// A JSON boolean.
    Bool(bool),
    /// JSON null.
    Null,
}

/// One `chat_template_kwargs`/`chat_template_args` value: a literal, or a
/// `$var` dispatch the request's thinking state fills (pi-ai
/// `ChatTemplateKwargValue`, dsh config.ts:247-256).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatTemplateKwargValue {
    /// A literal scalar sent as-is.
    Scalar(ScalarValue),
    /// A `$var` dispatch resolved per request.
    Dispatch {
        /// The dispatch key naming the request fact to fill in.
        var: ChatTemplateVar,
        /// Drop the entry entirely when the request selects no effort.
        omit_when_off: bool,
    },
}

/// The wire-compatibility switches a deployment states explicitly (dsh
/// `PiAiCompatProfile`).
///
/// This is the offered subset of pi-ai's compat types; every undecided
/// field falls to detection. Config keys render kebab-case (dsh camelCase
/// adapters).
///
/// Withheld pi-ai fields (`openRouterRouting`, `vercelGatewayRouting`,
/// `zaiToolStream`, `supportsOpenAIGrammarTools` — catalog.ts:268-275)
/// have no member here and stay false/empty on every resolved model, which
/// is the dsh behavior for hand-declared routes (Q74-B, decision 42).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompatProfile {
    /// `supports-store` — whether the endpoint accepts `store`.
    pub supports_store: Option<bool>,
    /// `supports-developer-role` — `developer` role for the system prompt.
    pub supports_developer_role: Option<bool>,
    /// `supports-reasoning-effort` — whether `reasoning_effort` is accepted.
    pub supports_reasoning_effort: Option<bool>,
    /// `supports-usage-in-streaming` — `stream_options: {include_usage}`.
    pub supports_usage_in_streaming: Option<bool>,
    /// `supports-finish-reason` — whether streams carry `finish_reason`.
    pub supports_finish_reason: Option<bool>,
    /// `max-tokens-field` — which output-cap field the endpoint reads.
    pub max_tokens_field: Option<MaxTokensField>,
    /// `requires-tool-result-name` — tool results must carry `name`.
    pub requires_tool_result_name: Option<bool>,
    /// `requires-assistant-after-tool-result` — bridge message required.
    pub requires_assistant_after_tool_result: Option<bool>,
    /// `requires-thinking-as-text` — thinking travels as plain text.
    pub requires_thinking_as_text: Option<bool>,
    /// `requires-reasoning-content-on-assistant-messages` — empty
    /// `reasoning_content` required on replayed assistant messages.
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    /// `thinking-format` — the reasoning-dispatch dialect.
    pub thinking_format: Option<ThinkingFormat>,
    /// `chat-template-kwargs` — `chat_template_kwargs` entries.
    pub chat_template_kwargs: Option<std::collections::BTreeMap<String, ChatTemplateKwargValue>>,
    /// `chat-template-args` — `chat_template_args` entries (baseten).
    pub chat_template_args: Option<std::collections::BTreeMap<String, ChatTemplateKwargValue>>,
    /// `supports-thinking-token-budget` — alias for the generic budget field.
    pub supports_thinking_token_budget: Option<bool>,
    /// `thinking-token-budget-field` — the budget request field.
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    /// `vllm-priority` — vLLM scheduler priority (integer).
    pub vllm_priority: Option<i64>,
    /// `supports-max-output-tokens` — responses `max_output_tokens`.
    pub supports_max_output_tokens: Option<bool>,
    /// `supports-strict-mode` — strict tool JSON schemas.
    pub supports_strict_mode: Option<bool>,
    /// `cache-control-format` — the prompt-cache marker convention.
    pub cache_control_format: Option<CacheControlFormat>,
    /// `supports-long-cache-retention` — long prompt-cache retention.
    pub supports_long_cache_retention: Option<bool>,
}

/// The fully resolved completions compat (pi-ai `ResolvedOpenAICompletionsCompat`
/// restricted to the reachable face of dsh `llm-pi-ai` serving hand-declared
/// routes).
///
/// Absent members and their disposition (each is the dsh behavior for this
/// configuration surface, PRD decision 42): `openRouterRouting`,
/// `vercelGatewayRouting`, `zaiToolStream`, and
/// `supportsOpenAIGrammarTools` are withheld (catalog.ts:268-275) and
/// unreachable through any fr profile, so they never influence the wire
/// request; `supports_mid_convo_system_messages` and
/// `supports_mid_convo_tool_additions` are withheld the same way and
/// detect as `false` (openai-completions.ts:1677-1678), which is the
/// Q74-B ruling.
#[expect(
    clippy::struct_excessive_bools,
    reason = "field set mirrors pi-ai ResolvedOpenAICompletionsCompat field-by-field (dsh compat gate, catalog.ts:248-307); merging or dropping fields would break the dsh wire-shape parity"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCompletionsCompat {
    /// Whether the endpoint accepts `store`.
    pub supports_store: bool,
    /// Whether the endpoint accepts the `developer` role for the system
    /// prompt (reasoning models only on the wire path).
    pub supports_developer_role: bool,
    /// Whether the endpoint accepts `reasoning_effort`.
    pub supports_reasoning_effort: bool,
    /// Whether `stream_options: {include_usage: true}` is accepted.
    pub supports_usage_in_streaming: bool,
    /// Whether streams carry `finish_reason`; `false` lets pi-ai's
    /// inference arm decide the terminal reason.
    pub supports_finish_reason: bool,
    /// Which output-cap field the endpoint reads.
    pub max_tokens_field: MaxTokensField,
    /// Whether tool results must carry `name`.
    pub requires_tool_result_name: bool,
    /// Whether a user message after tool results needs an assistant bridge.
    pub requires_assistant_after_tool_result: bool,
    /// Whether thinking blocks must travel as plain text.
    pub requires_thinking_as_text: bool,
    /// Whether replayed assistant messages need an empty `reasoning_content`.
    pub requires_reasoning_content_on_assistant_messages: bool,
    /// The reasoning-dispatch dialect.
    pub thinking_format: ThinkingFormat,
    /// `chat_template_kwargs` entries sent under the matching formats.
    pub chat_template_kwargs: std::collections::BTreeMap<String, ChatTemplateKwargValue>,
    /// `chat_template_args` entries sent under the baseten format.
    pub chat_template_args: std::collections::BTreeMap<String, ChatTemplateKwargValue>,
    /// Whether the generic thinking-token budget field is supported.
    pub supports_thinking_token_budget: bool,
    /// The budget request field; omitted unless configured.
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    /// The vLLM scheduler priority; omitted unless configured.
    pub vllm_priority: Option<i64>,
    /// Whether strict tool JSON schemas are accepted.
    pub supports_strict_mode: bool,
    /// The prompt-cache marker convention; omitted unless configured.
    pub cache_control_format: Option<CacheControlFormat>,
    /// Whether long prompt-cache retention is accepted.
    pub supports_long_cache_retention: bool,
    /// Whether session-affinity headers ride the request.
    pub send_session_affinity_headers: bool,
    /// The session-affinity header convention.
    pub session_affinity_format: SessionAffinityFormat,
    /// Q74-B: withheld on hand-declared routes, detect false — always
    /// `false` in this crate (no mid-conversation system messages).
    pub supports_mid_convo_system_messages: bool,
    /// Q74-B: withheld, detect false — always `false` (no tool-update
    /// capability).
    pub supports_mid_convo_tool_additions: bool,
}

/// The fully resolved responses compat (pi-ai `getCompat`,
/// openai-responses.ts:82-95).
///
/// `supports_additional_tools` and `supports_tool_search` are the Q74-B
/// face: withheld from configuration and detect `false`, so every
/// fr-llm-openai responses model lacks tool-update capability. The
/// grammar flag is the D8 exclusion (double-unreachable in dsh).
#[expect(
    clippy::struct_excessive_bools,
    reason = "field set mirrors pi-ai's resolved responses compat (openai-responses.ts:82-95, offered keys gated by catalog.ts:279-290), including the always-false Q74-B withheld fields; merging or dropping fields would break the dsh wire-shape parity"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResponsesCompat {
    /// Whether the `developer` role is accepted for instructions.
    pub supports_developer_role: bool,
    /// Q74-B: withheld, detect false — always `false`.
    pub supports_mid_convo_system_messages: bool,
    /// The session-affinity header convention (detected, withheld from
    /// configuration).
    pub session_affinity_format: SessionAffinityFormat,
    /// Whether long prompt-cache retention is accepted.
    pub supports_long_cache_retention: bool,
    /// Whether strict tool JSON schemas are accepted.
    pub supports_strict_mode: bool,
    /// D8: withheld and double-unreachable in dsh — always `false`.
    pub supports_openai_grammar_tools: bool,
    /// Q74-B: withheld, detect false — always `false`.
    pub supports_additional_tools: bool,
    /// Q74-B: withheld, detect false — always `false`.
    pub supports_tool_search: bool,
    /// Whether explicit prompt-cache modes are accepted.
    pub supports_explicit_prompt_cache_mode: bool,
    /// Whether `max_output_tokens` is accepted.
    pub supports_max_output_tokens: bool,
}

/// Detection outcome for the completions detection table (pi-ai
/// `detectCompat`, openai-completions.ts:1593-1690). Every field carries
/// the detected default before any profile entry lands on top.
#[expect(
    clippy::struct_excessive_bools,
    reason = "field set mirrors the pi-ai detectCompat detection table (openai-completions.ts:1593-1690) field-by-field; merging or dropping fields would break the dsh detection parity"
)]
#[derive(Debug, Clone, PartialEq)]
struct DetectedCompat {
    supports_store: bool,
    supports_developer_role: bool,
    supports_reasoning_effort: bool,
    supports_usage_in_streaming: bool,
    supports_finish_reason: bool,
    max_tokens_field: MaxTokensField,
    requires_tool_result_name: bool,
    requires_assistant_after_tool_result: bool,
    requires_thinking_as_text: bool,
    requires_reasoning_content_on_assistant_messages: bool,
    thinking_format: ThinkingFormat,
    supports_thinking_token_budget: bool,
    supports_strict_mode: bool,
    cache_control_format: Option<CacheControlFormat>,
    supports_long_cache_retention: bool,
    send_session_affinity_headers: bool,
    session_affinity_format: SessionAffinityFormat,
}

/// Detects completions compat from the route id, endpoint, and model id
/// (the pi-ai detection table, verbatim).
fn detect_compat(provider: &str, base_url: &str, model_id: &str) -> DetectedCompat {
    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai"
        || provider == "moonshotai-cn"
        || base_url.contains("api.moonshot.");
    let is_openrouter = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers_ai =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_cerebras = provider == "cerebras" || base_url.contains("cerebras.ai");
    // JS `toLowerCase().includes` parity: fold the URL before matching.
    let is_deepseek = provider == "deepseek" || base_url.to_lowercase().contains("deepseek.com");
    let is_non_standard = is_nvidia
        || is_cerebras
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;
    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || (is_cloudflare_ai_gateway || is_together || is_nvidia || is_ant_ling || is_zai);
    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_openrouter_developer_role_model =
        is_openrouter && (model_id.starts_with("anthropic/") || model_id.starts_with("openai/"));
    DetectedCompat {
        supports_store: !is_non_standard,
        supports_developer_role: is_openrouter_developer_role_model
            || (!is_non_standard && !is_openrouter),
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format: if is_deepseek {
            ThinkingFormat::Deepseek
        } else if is_zai {
            ThinkingFormat::Zai
        } else if is_together {
            ThinkingFormat::Together
        } else if is_ant_ling {
            ThinkingFormat::AntLing
        } else if is_openrouter {
            ThinkingFormat::Openrouter
        } else {
            ThinkingFormat::Openai
        },
        supports_thinking_token_budget: false,
        supports_strict_mode: false,
        cache_control_format: (provider == "openrouter" && model_id.starts_with("anthropic/"))
            .then_some(CacheControlFormat::Anthropic),
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
        send_session_affinity_headers: is_openrouter,
        session_affinity_format: if is_openrouter {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        },
    }
}

/// Detects the responses session-affinity convention (pi-ai
/// `detectSessionAffinityFormat`, openai-responses.ts:64-66).
fn detect_session_affinity(provider: &str, base_url: &str) -> SessionAffinityFormat {
    if provider == "openrouter" || base_url.contains("openrouter.ai") {
        SessionAffinityFormat::Openrouter
    } else {
        SessionAffinityFormat::Openai
    }
}

/// Lands profile entries over the detected base, field by field (pi-ai
/// `getCompat` merge; model-level entries win over route-level entries
/// because they are applied later).
fn apply_completions_profile(
    mut base: ResolvedCompletionsCompat,
    profile: Option<&CompatProfile>,
) -> ResolvedCompletionsCompat {
    let Some(p) = profile else {
        return base;
    };
    if let Some(v) = p.supports_store {
        base.supports_store = v;
    }
    if let Some(v) = p.supports_developer_role {
        base.supports_developer_role = v;
    }
    if let Some(v) = p.supports_reasoning_effort {
        base.supports_reasoning_effort = v;
    }
    if let Some(v) = p.supports_usage_in_streaming {
        base.supports_usage_in_streaming = v;
    }
    if let Some(v) = p.supports_finish_reason {
        base.supports_finish_reason = v;
    }
    if let Some(v) = p.max_tokens_field {
        base.max_tokens_field = v;
    }
    if let Some(v) = p.requires_tool_result_name {
        base.requires_tool_result_name = v;
    }
    if let Some(v) = p.requires_assistant_after_tool_result {
        base.requires_assistant_after_tool_result = v;
    }
    if let Some(v) = p.requires_thinking_as_text {
        base.requires_thinking_as_text = v;
    }
    if let Some(v) = p.requires_reasoning_content_on_assistant_messages {
        base.requires_reasoning_content_on_assistant_messages = v;
    }
    if let Some(v) = p.thinking_format {
        base.thinking_format = v;
    }
    if let Some(v) = p.chat_template_kwargs.clone() {
        base.chat_template_kwargs = v;
    }
    if let Some(v) = p.chat_template_args.clone() {
        base.chat_template_args = v;
    }
    if let Some(v) = p.supports_thinking_token_budget {
        base.supports_thinking_token_budget = v;
    }
    if let Some(v) = p.thinking_token_budget_field {
        base.thinking_token_budget_field = Some(v);
    }
    if let Some(v) = p.vllm_priority {
        base.vllm_priority = Some(v);
    }
    if let Some(v) = p.supports_strict_mode {
        base.supports_strict_mode = v;
    }
    if let Some(v) = p.cache_control_format {
        base.cache_control_format = Some(v);
    }
    if let Some(v) = p.supports_long_cache_retention {
        base.supports_long_cache_retention = v;
    }
    base
}

/// Resolves the responses compat from the offered profile keys
/// (`supports-developer-role`/`supports-max-output-tokens`/
/// `supports-strict-mode`/`supports-long-cache-retention`) and the
/// detected session-affinity convention; everything else lands on the
/// withheld defaults (Q74-B/D8).
fn resolve_responses_compat(
    profile: Option<&CompatProfile>,
    session_affinity: SessionAffinityFormat,
) -> ResolvedResponsesCompat {
    let p = profile;
    ResolvedResponsesCompat {
        supports_developer_role: p.and_then(|p| p.supports_developer_role).unwrap_or(true),
        supports_mid_convo_system_messages: false,
        session_affinity_format: session_affinity,
        supports_long_cache_retention: p
            .and_then(|p| p.supports_long_cache_retention)
            .unwrap_or(true),
        supports_strict_mode: p.and_then(|p| p.supports_strict_mode).unwrap_or(false),
        supports_openai_grammar_tools: false,
        supports_additional_tools: false,
        supports_tool_search: false,
        supports_explicit_prompt_cache_mode: false,
        supports_max_output_tokens: p.and_then(|p| p.supports_max_output_tokens).unwrap_or(true),
    }
}

/// The compat fields dsh's configuration surface offers for the
/// completions protocol (the offer arm of `COMPLETIONS_COMPAT_GATE`,
/// catalog.ts:248-267, rendered in fr kebab-case).
pub(crate) const COMPLETIONS_OFFERED: &[&str] = &[
    "supports-store",
    "supports-developer-role",
    "supports-reasoning-effort",
    "supports-usage-in-streaming",
    "supports-finish-reason",
    "max-tokens-field",
    "requires-tool-result-name",
    "requires-assistant-after-tool-result",
    "requires-thinking-as-text",
    "requires-reasoning-content-on-assistant-messages",
    "thinking-format",
    "chat-template-kwargs",
    "chat-template-args",
    "supports-thinking-token-budget",
    "thinking-token-budget-field",
    "vllm-priority",
    "supports-strict-mode",
    "cache-control-format",
    "supports-long-cache-retention",
];

/// The compat fields dsh's configuration surface offers for the responses
/// protocol (the offer arm of `RESPONSES_COMPAT_GATE`, catalog.ts:279-290).
pub(crate) const RESPONSES_OFFERED: &[&str] = &[
    "supports-developer-role",
    "supports-max-output-tokens",
    "supports-strict-mode",
    "supports-long-cache-retention",
];

/// The protocols whose compat type takes one configured field (dsh
/// `compatProtocols`, catalog.ts:527-529); empty for a withheld or
/// undeclared name.
pub(crate) fn compat_field_protocols(field: &str) -> Vec<&'static str> {
    let mut protocols = Vec::new();
    if COMPLETIONS_OFFERED.contains(&field) {
        protocols.push(Api::OpenaiCompletions.as_str());
    }
    if RESPONSES_OFFERED.contains(&field) {
        protocols.push(Api::OpenaiResponses.as_str());
    }
    protocols
}

/// Every offered field name, deduplicated in gate order (dsh
/// `allOfferedCompatFields`, catalog.ts:548-554).
pub(crate) fn all_offered_compat_fields() -> Vec<&'static str> {
    let mut fields = Vec::new();
    for field in COMPLETIONS_OFFERED
        .iter()
        .copied()
        .chain(RESPONSES_OFFERED.iter().copied())
    {
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    fields
}

/// The offered fields for one protocol, for the diagnostic that shows what
/// was available instead of the name that missed (dsh
/// `offeredCompatFields`, catalog.ts:537-539).
pub(crate) fn offered_compat_fields(api: Api) -> Vec<&'static str> {
    match api {
        Api::OpenaiCompletions => COMPLETIONS_OFFERED.to_vec(),
        Api::OpenaiResponses => RESPONSES_OFFERED.to_vec(),
    }
}

/// The compat field names a profile actually sets (fr kebab-case
/// spellings), in declaration order — the vocabulary the route-tail and
/// per-model gate checks walk.
pub(crate) fn configured_compat_fields(profile: &CompatProfile) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if profile.supports_store.is_some() {
        fields.push("supports-store");
    }
    if profile.supports_developer_role.is_some() {
        fields.push("supports-developer-role");
    }
    if profile.supports_reasoning_effort.is_some() {
        fields.push("supports-reasoning-effort");
    }
    if profile.supports_usage_in_streaming.is_some() {
        fields.push("supports-usage-in-streaming");
    }
    if profile.supports_finish_reason.is_some() {
        fields.push("supports-finish-reason");
    }
    if profile.max_tokens_field.is_some() {
        fields.push("max-tokens-field");
    }
    if profile.requires_tool_result_name.is_some() {
        fields.push("requires-tool-result-name");
    }
    if profile.requires_assistant_after_tool_result.is_some() {
        fields.push("requires-assistant-after-tool-result");
    }
    if profile.requires_thinking_as_text.is_some() {
        fields.push("requires-thinking-as-text");
    }
    if profile
        .requires_reasoning_content_on_assistant_messages
        .is_some()
    {
        fields.push("requires-reasoning-content-on-assistant-messages");
    }
    if profile.thinking_format.is_some() {
        fields.push("thinking-format");
    }
    if profile.chat_template_kwargs.is_some() {
        fields.push("chat-template-kwargs");
    }
    if profile.chat_template_args.is_some() {
        fields.push("chat-template-args");
    }
    if profile.supports_thinking_token_budget.is_some() {
        fields.push("supports-thinking-token-budget");
    }
    if profile.thinking_token_budget_field.is_some() {
        fields.push("thinking-token-budget-field");
    }
    if profile.vllm_priority.is_some() {
        fields.push("vllm-priority");
    }
    if profile.supports_max_output_tokens.is_some() {
        fields.push("supports-max-output-tokens");
    }
    if profile.supports_strict_mode.is_some() {
        fields.push("supports-strict-mode");
    }
    if profile.cache_control_format.is_some() {
        fields.push("cache-control-format");
    }
    if profile.supports_long_cache_retention.is_some() {
        fields.push("supports-long-cache-retention");
    }
    fields
}

/// The resolved compat block a materialized model carries, keyed by the
/// protocol the model speaks (dsh `ModelCompat` restricted to the two
/// served protocols).
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedCompat {
    /// Completions-wire compat.
    Completions(ResolvedCompletionsCompat),
    /// Responses-wire compat.
    Responses(ResolvedResponsesCompat),
}

/// Resolves one completions model's compat: detection, then the route
/// profile, then the model profile (model entries win field by field).
pub(crate) fn resolved_completions_compat(
    provider: &str,
    base_url: &str,
    model_id: &str,
    route: Option<&CompatProfile>,
    model: Option<&CompatProfile>,
) -> ResolvedCompletionsCompat {
    let detected = detect_compat(provider, base_url, model_id);
    let base = ResolvedCompletionsCompat {
        supports_store: detected.supports_store,
        supports_developer_role: detected.supports_developer_role,
        supports_reasoning_effort: detected.supports_reasoning_effort,
        supports_usage_in_streaming: detected.supports_usage_in_streaming,
        supports_finish_reason: detected.supports_finish_reason,
        max_tokens_field: detected.max_tokens_field,
        requires_tool_result_name: detected.requires_tool_result_name,
        requires_assistant_after_tool_result: detected.requires_assistant_after_tool_result,
        requires_thinking_as_text: detected.requires_thinking_as_text,
        requires_reasoning_content_on_assistant_messages: detected
            .requires_reasoning_content_on_assistant_messages,
        thinking_format: detected.thinking_format,
        chat_template_kwargs: std::collections::BTreeMap::new(),
        chat_template_args: std::collections::BTreeMap::new(),
        supports_thinking_token_budget: detected.supports_thinking_token_budget,
        // Withheld from configuration (catalog.ts:273-275) and never set by
        // detection, so the resolved value is the unset face.
        thinking_token_budget_field: None,
        vllm_priority: None,
        supports_strict_mode: detected.supports_strict_mode,
        cache_control_format: detected.cache_control_format,
        supports_long_cache_retention: detected.supports_long_cache_retention,
        send_session_affinity_headers: detected.send_session_affinity_headers,
        session_affinity_format: detected.session_affinity_format,
        supports_mid_convo_system_messages: false,
        supports_mid_convo_tool_additions: false,
    };
    apply_completions_profile(apply_completions_profile(base, route), model)
}

/// Resolves one responses model's compat (the four offered keys over the
/// withheld defaults; session affinity comes from detection).
pub(crate) fn resolved_responses_compat(
    provider: &str,
    base_url: &str,
    route: Option<&CompatProfile>,
    model: Option<&CompatProfile>,
) -> ResolvedResponsesCompat {
    let affinity = detect_session_affinity(provider, base_url);
    resolve_responses_compat(route, affinity).merge_model(model)
}

impl ResolvedResponsesCompat {
    const fn merge_model(mut self, model: Option<&CompatProfile>) -> Self {
        let Some(p) = model else {
            return self;
        };
        if let Some(v) = p.supports_developer_role {
            self.supports_developer_role = v;
        }
        if let Some(v) = p.supports_max_output_tokens {
            self.supports_max_output_tokens = v;
        }
        if let Some(v) = p.supports_strict_mode {
            self.supports_strict_mode = v;
        }
        if let Some(v) = p.supports_long_cache_retention {
            self.supports_long_cache_retention = v;
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Configuration parsing (dsh `assertOfferedCompatFields` + schemastery
// member schemas, catalog.ts:566-600 and config.ts:258-300). Diagnostic
// wording follows the batch adaptation note in the module header: the
// owning-plugin prefix renders as `fr-llm-openai`, config keys render
// kebab-case, and the two texts that point at dsh's installed catalog are
// reworded for the catalog-less fr surface (recorded at slice close).
// ---------------------------------------------------------------------------

/// The compat fields pi-ai's compat types declare but dsh withholds from
/// its configuration surface (catalog.ts:268-290 withhold arms, kebab
/// spellings); they are refused wherever written.
pub(crate) const WITHHELD_COMPAT_FIELDS: &[&str] = &[
    "open-router-routing",
    "vercel-gateway-routing",
    "zai-tool-stream",
    "supports-openai-grammar-tools",
    "send-session-affinity-headers",
    "supports-mid-convo-system-messages",
    "supports-mid-convo-tool-additions",
    "session-affinity-format",
    "supports-additional-tools",
    "supports-tool-search",
    "supports-explicit-prompt-cache-mode",
];

/// Parses one boolean compat member.
fn parse_compat_bool(field: &str, value: &serde_json::Value) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("compat \"{field}\" must be a boolean"))
}

/// Parses one `max-tokens-field` member.
fn parse_max_tokens_field(
    field: &str,
    value: &serde_json::Value,
) -> Result<MaxTokensField, String> {
    match value.as_str() {
        Some("max_completion_tokens") => Ok(MaxTokensField::MaxCompletionTokens),
        Some("max_tokens") => Ok(MaxTokensField::MaxTokens),
        _ => Err(format!(
            "compat \"{field}\" must be \"max_completion_tokens\" or \"max_tokens\""
        )),
    }
}

/// Parses one `thinking-format` member.
fn parse_thinking_format(field: &str, value: &serde_json::Value) -> Result<ThinkingFormat, String> {
    let literal = value.as_str().ok_or_else(|| {
        format!("compat \"{field}\" must name one of the reasoning-dispatch dialects")
    })?;
    let format = match literal {
        "openai" => ThinkingFormat::Openai,
        "deepseek" => ThinkingFormat::Deepseek,
        "openrouter" => ThinkingFormat::Openrouter,
        "together" => ThinkingFormat::Together,
        "baseten" => ThinkingFormat::Baseten,
        "zai" => ThinkingFormat::Zai,
        "qwen" => ThinkingFormat::Qwen,
        "chat-template" => ThinkingFormat::ChatTemplate,
        "qwen-chat-template" => ThinkingFormat::QwenChatTemplate,
        "string-thinking" => ThinkingFormat::StringThinking,
        "ant-ling" => ThinkingFormat::AntLing,
        other => {
            return Err(format!(
                "compat \"{field}\" names unknown dialect \"{other}\""
            ));
        }
    };
    Ok(format)
}

/// Parses one `thinking-token-budget-field` member.
fn parse_thinking_budget_field(
    field: &str,
    value: &serde_json::Value,
) -> Result<ThinkingTokenBudgetField, String> {
    match value.as_str() {
        Some("thinking_token_budget") => Ok(ThinkingTokenBudgetField::ThinkingTokenBudget),
        Some("thinking_budget") => Ok(ThinkingTokenBudgetField::ThinkingBudget),
        Some("thinking_budget_tokens") => Ok(ThinkingTokenBudgetField::ThinkingBudgetTokens),
        _ => Err(format!(
            "compat \"{field}\" must be \"thinking_token_budget\", \"thinking_budget\", or \
             \"thinking_budget_tokens\""
        )),
    }
}

/// Parses one `cache-control-format` member.
fn parse_cache_control_format(
    field: &str,
    value: &serde_json::Value,
) -> Result<CacheControlFormat, String> {
    match value.as_str() {
        Some("anthropic") => Ok(CacheControlFormat::Anthropic),
        _ => Err(format!("compat \"{field}\" must be \"anthropic\"")),
    }
}

/// Parses one chat-template kwarg value: a literal scalar, or a `$var`
/// dispatch object (dsh `chatTemplateKwarg` union, config.ts:247-256).
pub(crate) fn parse_chat_template_kwarg(
    value: &serde_json::Value,
) -> Result<ChatTemplateKwargValue, String> {
    match value {
        serde_json::Value::String(text) => Ok(ChatTemplateKwargValue::Scalar(ScalarValue::Str(
            text.clone(),
        ))),
        serde_json::Value::Number(number) => {
            let scalar = number
                .as_f64()
                .map(ScalarValue::Num)
                .ok_or_else(|| "chat-template value must be a finite number".to_owned())?;
            Ok(ChatTemplateKwargValue::Scalar(scalar))
        }
        serde_json::Value::Bool(flag) => {
            Ok(ChatTemplateKwargValue::Scalar(ScalarValue::Bool(*flag)))
        }
        serde_json::Value::Null => Ok(ChatTemplateKwargValue::Scalar(ScalarValue::Null)),
        serde_json::Value::Object(map) => {
            let var_literal = map
                .get("$var")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "chat-template dispatch object must carry \"$var\"".to_owned())?;
            let var = match var_literal {
                "thinking.enabled" => ChatTemplateVar::ThinkingEnabled,
                "thinking.effort" => ChatTemplateVar::ThinkingEffort,
                "thinking.budget" => ChatTemplateVar::ThinkingBudget,
                other => {
                    return Err(format!(
                        "chat-template dispatch \"$var\" must be one of thinking.enabled, \
                         thinking.effort, thinking.budget; got \"{other}\""
                    ));
                }
            };
            let omit_when_off = match map.get("omit-when-off") {
                Some(value) => value.as_bool().ok_or_else(|| {
                    "chat-template dispatch \"omit-when-off\" must be a boolean".to_owned()
                })?,
                None => false,
            };
            Ok(ChatTemplateKwargValue::Dispatch { var, omit_when_off })
        }
        serde_json::Value::Array(_) => {
            Err("chat-template value must be a scalar or a dispatch object".to_owned())
        }
    }
}

/// Parses one compat profile (route-level or model-level) from raw
/// configuration (dsh `compatProfile` schema + `assertOfferedCompatFields`,
/// catalog.ts:566-600).
///
/// The vocabulary check runs before any member parse: a withheld name is
/// refused wherever written, then a valueless key, then the typed members.
///
/// # Errors
///
/// Names the offending key: withheld (`not configurable here`), undeclared
/// (`which no wire protocol declares`), valueless, or a member with a
/// malformed value.
/// Lands one typed compat member onto the profile (the member arms of
/// dsh `compatProfile`, config.ts:258-285).
fn apply_compat_member(
    profile: &mut CompatProfile,
    provider: &str,
    site: &str,
    key: &str,
    member: &serde_json::Value,
) -> Result<(), String> {
    match key {
        "supports-store" => {
            profile.supports_store = Some(parse_compat_bool(key, member)?);
        }
        "supports-developer-role" => {
            profile.supports_developer_role = Some(parse_compat_bool(key, member)?);
        }
        "supports-reasoning-effort" => {
            profile.supports_reasoning_effort = Some(parse_compat_bool(key, member)?);
        }
        "supports-usage-in-streaming" => {
            profile.supports_usage_in_streaming = Some(parse_compat_bool(key, member)?);
        }
        "supports-finish-reason" => {
            profile.supports_finish_reason = Some(parse_compat_bool(key, member)?);
        }
        "max-tokens-field" => {
            profile.max_tokens_field = Some(parse_max_tokens_field(key, member)?);
        }
        "requires-tool-result-name" => {
            profile.requires_tool_result_name = Some(parse_compat_bool(key, member)?);
        }
        "requires-assistant-after-tool-result" => {
            profile.requires_assistant_after_tool_result = Some(parse_compat_bool(key, member)?);
        }
        "requires-thinking-as-text" => {
            profile.requires_thinking_as_text = Some(parse_compat_bool(key, member)?);
        }
        "requires-reasoning-content-on-assistant-messages" => {
            profile.requires_reasoning_content_on_assistant_messages =
                Some(parse_compat_bool(key, member)?);
        }
        "thinking-format" => {
            profile.thinking_format = Some(parse_thinking_format(key, member)?);
        }
        "chat-template-kwargs" => {
            profile.chat_template_kwargs = Some(parse_chat_template_map(key, member)?);
        }
        "chat-template-args" => {
            profile.chat_template_args = Some(parse_chat_template_map(key, member)?);
        }
        "supports-thinking-token-budget" => {
            profile.supports_thinking_token_budget = Some(parse_compat_bool(key, member)?);
        }
        "thinking-token-budget-field" => {
            profile.thinking_token_budget_field = Some(parse_thinking_budget_field(key, member)?);
        }
        "vllm-priority" => {
            let priority = member
                .as_i64()
                .ok_or_else(|| format!("compat \"{key}\" must be an integer"))?;
            profile.vllm_priority = Some(priority);
        }
        "supports-max-output-tokens" => {
            profile.supports_max_output_tokens = Some(parse_compat_bool(key, member)?);
        }
        "supports-strict-mode" => {
            profile.supports_strict_mode = Some(parse_compat_bool(key, member)?);
        }
        "cache-control-format" => {
            profile.cache_control_format = Some(parse_cache_control_format(key, member)?);
        }
        "supports-long-cache-retention" => {
            profile.supports_long_cache_retention = Some(parse_compat_bool(key, member)?);
        }
        // The vocabulary gate in `parse_compat_profile` admits only offered
        // keys, and every offered key has an arm, so this arm fails closed
        // on a build defect instead of panicking.
        other => {
            return Err(invalid_compat(
                provider,
                site,
                &format!("compat \"{other}\" is offered but has no parse arm; build defect"),
            ));
        }
    }
    Ok(())
}

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
        if compat_field_protocols(key).is_empty() {
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
                    all_offered_compat_fields().join(", ")
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
        apply_compat_member(&mut profile, provider, site, key.as_str(), member)?;
    }
    Ok(profile)
}

/// Parses one chat-template map member (`chat-template-kwargs` /
/// `chat-template-args`).
fn parse_chat_template_map(
    field: &str,
    value: &serde_json::Value,
) -> Result<std::collections::BTreeMap<String, ChatTemplateKwargValue>, String> {
    let serde_json::Value::Object(map) = value else {
        return Err(format!("compat \"{field}\" must be an object"));
    };
    let mut parsed = std::collections::BTreeMap::new();
    for (name, entry) in map {
        parsed.insert(name.clone(), parse_chat_template_kwarg(entry)?);
    }
    Ok(parsed)
}

/// Formats one compat-site diagnostic with the fr artifact prefix (dsh
/// `invalid`, catalog.ts:676-678, compat-site shape).
fn invalid_compat(provider: &str, site: &str, detail: &str) -> String {
    format!("fr-llm-openai: provider \"{provider}\" {site} {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_unknown_gateway_matches_openai_baseline() {
        let c =
            resolved_completions_compat("acme", "https://gw.example/v1", "acme-large", None, None);
        assert_eq!(c.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        assert_eq!(c.thinking_format, ThinkingFormat::Openai);
        assert!(c.supports_store);
        assert!(!c.supports_strict_mode);
        assert_eq!(c.session_affinity_format, SessionAffinityFormat::Openai);
        assert!(c.supports_long_cache_retention);
        assert!(!c.supports_mid_convo_system_messages);
        assert!(!c.supports_mid_convo_tool_additions);
    }

    #[test]
    fn detect_deepseek_route_pins_max_tokens_and_dialect() {
        let c = resolved_completions_compat(
            "deepseek",
            "https://api.deepseek.com/v1",
            "ds-chat",
            None,
            None,
        );
        assert_eq!(c.max_tokens_field, MaxTokensField::MaxTokens);
        assert_eq!(c.thinking_format, ThinkingFormat::Deepseek);
        assert!(c.requires_reasoning_content_on_assistant_messages);
        assert!(!c.supports_store);
        assert!(c.supports_long_cache_retention);
        assert!(!c.send_session_affinity_headers);
    }

    #[test]
    fn detect_openrouter_carries_affinity_and_cache_control() {
        let c = resolved_completions_compat(
            "openrouter",
            "https://openrouter.ai/api/v1",
            "anthropic/x",
            None,
            None,
        );
        assert!(c.send_session_affinity_headers);
        assert_eq!(c.session_affinity_format, SessionAffinityFormat::Openrouter);
        assert_eq!(c.cache_control_format, Some(CacheControlFormat::Anthropic));
        assert_eq!(c.thinking_format, ThinkingFormat::Openrouter);
        assert!(c.supports_developer_role);
    }

    #[test]
    fn profile_entries_win_field_by_field() {
        let route = CompatProfile {
            max_tokens_field: Some(MaxTokensField::MaxCompletionTokens),
            supports_strict_mode: Some(true),
            ..CompatProfile::default()
        };
        let model = CompatProfile {
            supports_strict_mode: Some(false),
            ..CompatProfile::default()
        };
        let c = resolved_completions_compat(
            "deepseek",
            "https://api.deepseek.com/v1",
            "ds-chat",
            Some(&route),
            Some(&model),
        );
        assert_eq!(c.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        assert!(!c.supports_strict_mode);
        assert_eq!(c.thinking_format, ThinkingFormat::Deepseek);
    }

    #[test]
    fn responses_compat_defaults_and_offered_keys() {
        let route = CompatProfile {
            supports_strict_mode: Some(true),
            supports_max_output_tokens: Some(false),
            ..CompatProfile::default()
        };
        let c =
            resolved_responses_compat("openrouter", "https://openrouter.ai/v1", Some(&route), None);
        assert!(c.supports_developer_role);
        assert!(c.supports_strict_mode);
        assert!(!c.supports_max_output_tokens);
        assert!(c.supports_long_cache_retention);
        assert!(!c.supports_additional_tools);
        assert!(!c.supports_tool_search);
        assert_eq!(c.session_affinity_format, SessionAffinityFormat::Openrouter);
        let plain = resolved_responses_compat("acme", "https://gw.example/v1", None, None);
        assert_eq!(plain.session_affinity_format, SessionAffinityFormat::Openai);
    }

    #[test]
    fn compat_field_protocols_split_the_shared_keys() {
        assert_eq!(
            compat_field_protocols("supports-strict-mode"),
            vec!["openai-completions", "openai-responses"]
        );
        assert_eq!(
            compat_field_protocols("thinking-format"),
            vec!["openai-completions"]
        );
        assert_eq!(
            compat_field_protocols("supports-additional-tools"),
            [] as [&str; 0]
        );
        let all = all_offered_compat_fields();
        assert!(all.contains(&"supports-strict-mode"));
        assert!(all.contains(&"thinking-format"));
        assert_eq!(all.len(), 20);
    }
}

// Rust guideline compliant 2026-10-26
