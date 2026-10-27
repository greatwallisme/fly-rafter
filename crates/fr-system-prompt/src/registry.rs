// Rust guideline compliant 2026-10-28
//! The prompt registry: ordered section/context registration, variables,
//! tool-schema providers, and assembly (dsh `SystemPrompt`, verbatim
//! semantics).
//!
//! Fr mapping of the dsh registration surface (Q86-A): dynamic text,
//! variable, and tool providers are callback endpoints — a
//! `(service, method)` pair the caller resolves at each assembly through
//! the [`ProviderResolver`] seam; static text registers inline. The
//! `system-prompt/assemble` waterfall is an in-process listener seam
//! (Q87-A; M1 has no cross-plugin listeners), and a listener that drops
//! its `next` token vetoes the rest of the chain, exactly like dsh.
//!
//! Scoped layers are structurally absent: dougong activates each plugin
//! once with no per-session realm (PRD 19.2 item 24), so one registry is
//! one global layer; the dsh duplicate-name message keeps its `agent.ctx`
//! hint verbatim.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::interpolate::{
    AssembledContext, AssembledSection, PromptAssembly, PromptError, Variables,
};
use crate::orders::{CONTEXT_ORDERS, SECTION_ORDERS};
use crate::prompts::{HARNESS_IDENTITY_TEXT, PERSONA_PREFIX_SECTION, PERSONA_SUFFIX_SECTION};
use crate::tool_order::{order_tools, validate_tool_order};

/// A `(service, method)` callback endpoint carrying one dynamic provider
/// (Q86-A); the registry resolves it at each assembly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointRef {
    /// The providing dougong service name.
    pub service: String,
    /// The method selector on that service.
    pub method: u32,
}

/// One section's text: static, or a provider endpoint evaluated per
/// assembly. On the wire a static text is a bare string; an endpoint is
/// the `textProvider` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SectionText {
    /// Static text.
    Literal(String),
    /// A callback endpoint returning the text for one assembly.
    #[serde(rename_all = "camelCase")]
    Endpoint {
        /// The provider endpoint.
        text_provider: EndpointRef,
    },
}

/// One variable's source: a static value, or a provider endpoint that may
/// return `None` for a valueless assembly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum VariableSource {
    /// A static value.
    Literal {
        /// The value this variable always resolves to.
        value: String,
    },
    /// A callback endpoint returning the value (or none) per assembly.
    #[serde(rename_all = "camelCase")]
    Endpoint {
        /// The provider endpoint.
        value_provider: EndpointRef,
    },
}

/// One tool-schema provider registration (dsh `tools(provider)`); always
/// a callback endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProviderRegistration {
    /// The provider endpoint.
    pub provider: EndpointRef,
}

/// One section registration (dsh `PromptSection`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSectionSpec {
    /// Unique name; a duplicate registration throws.
    pub name: String,
    /// Sections concatenate in ascending order; equal orders use
    /// code-unit name order.
    pub order: f64,
    /// Static text or a per-assembly provider.
    pub text: SectionText,
    /// Whether to interpolate prompt variables; absent means true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpolate: Option<bool>,
    /// Treat this contribution as the complete system prompt; more than
    /// one effective complete section fails assembly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete: Option<bool>,
}

/// One context registration (dsh `PromptContext`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptContextSpec {
    /// Unique name; a duplicate registration throws.
    pub name: String,
    /// Contexts join in ascending order.
    pub order: f64,
    /// Static text or a per-assembly provider; empty text contributes
    /// nothing.
    pub text: SectionText,
    /// Whether to interpolate prompt variables; absent means true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpolate: Option<bool>,
    /// Keep this operational context when optional runtime context is
    /// disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// Tool schemas visible in one assembly and their pre-restriction name
/// universe (dsh `ToolProviderResult`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "ToolSchema embeds serde_json::Map (f64-containing Value), so Eq is not derivable"
)]
#[serde(rename_all = "camelCase")]
pub struct ToolProviderResult {
    /// The schemas this provider contributes to this assembly.
    pub schemas: Vec<fr_wire::call_config::ToolSchema>,
    /// The pre-restriction name universe for config validation; defaults
    /// to this result's schema names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_names: Option<Vec<String>>,
}

/// The per-assembly caller facts (dsh `AssembleContext` reduced to its
/// wire-carriable shape; scope and signal are structurally absent — PRD
/// 19.2 item 24 and the B2 abort ruling).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireAssembleContext {
    /// The requesting agent's options, when an agent is bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<WireAgentFacts>,
}

/// The agent facts prompt providers may read (dsh
/// `context.agent?.options`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireAgentFacts {
    /// The configured provider id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The configured model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Registry configuration (dsh `Config`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryConfig {
    /// Include the fixed harness identity before the deployment persona
    /// (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_harness_identity: Option<bool>,
    /// Include optional runtime-context snapshots in model history
    /// (default true); required context remains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_runtime_context: Option<bool>,
    /// Deployment-wide persona prefix template before first-party
    /// guidance (default `''`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_prefix: Option<String>,
    /// Persona suffix template after first-party guidance (default `''`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_suffix: Option<String>,
    /// Model-facing tool names in order, with the rest marker exactly
    /// once; omitted means lexicographic order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_order: Option<Vec<String>>,
}

/// Resolves callback endpoints during one assembly (Q86-A). Implementations
/// perform the outbound `require`; tests drive stubs.
pub trait ProviderResolver {
    /// Evaluates one text provider endpoint.
    ///
    /// # Errors
    ///
    /// Returns the provider failure verbatim.
    fn text(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<String, String>;

    /// Evaluates one variable provider endpoint; `Ok(None)` marks a
    /// registered name without a value for this assembly.
    ///
    /// # Errors
    ///
    /// Returns the provider failure verbatim.
    fn variable(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<Option<String>, String>;

    /// Evaluates one tool-schema provider endpoint.
    ///
    /// # Errors
    ///
    /// Returns the provider failure verbatim.
    fn tools(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<ToolProviderResult, String>;
}

/// One `system-prompt/assemble` waterfall listener (Q87-A in-process
/// seam). Dropping `next` vetoes the rest of the chain and this
/// listener's return value becomes authoritative.
pub type AssembleListener = Arc<
    dyn for<'a> Fn(
            PromptAssembly,
            &'a WireAssembleContext,
            AssembleNext<'a>,
        ) -> Result<PromptAssembly, String>
        + Send
        + Sync,
>;

/// The continuation token one waterfall listener either calls or drops.
pub type AssembleNext<'a> = Box<dyn FnOnce(PromptAssembly) -> Result<PromptAssembly, String> + 'a>;

/// Disposer handle for one tool-provider registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolProviderToken(u64);

impl ToolProviderToken {
    /// Rebuilds a token from its wire value (the unregister round trip).
    #[must_use]
    pub const fn from_raw(id: u64) -> Self {
        Self(id)
    }

    /// The token's wire value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Disposer handle for one runtime-context suppression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuppressionToken(u64);

impl SuppressionToken {
    /// Rebuilds a token from its wire value (the release round trip).
    #[must_use]
    pub const fn from_raw(id: u64) -> Self {
        Self(id)
    }
}

/// The registered identity name (dsh `harness:identity`).
const HARNESS_IDENTITY_SECTION: &str = "harness:identity";

/// The suffix appended to the dsh duplicate-name messages; dsh points
/// global re-registrations at the per-agent scope override, which fr
/// keeps verbatim.
const DUPLICATE_HINT: &str =
    " (for a per-agent override, register through that agent's `agent.ctx` instead)";

/// The registry (dsh `SystemPrompt`): one global layer.
#[derive(Default)]
pub struct SystemPromptRegistry {
    include_runtime_context: bool,
    persona_prefix: String,
    persona_suffix: String,
    tool_order: Option<Vec<String>>,
    sections: IndexMap<String, PromptSectionSpec>,
    contexts: IndexMap<String, PromptContextSpec>,
    variables: IndexMap<String, VariableSource>,
    tool_providers: IndexMap<u64, ToolProviderRegistration>,
    next_tool_provider: u64,
    suppressions: Vec<u64>,
    next_suppression: u64,
    waterfall: Vec<AssembleListener>,
}

impl std::fmt::Debug for SystemPromptRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemPromptRegistry")
            .field("include runtime context", &self.include_runtime_context)
            .field("persona prefix", &self.persona_prefix)
            .field("persona suffix", &self.persona_suffix)
            .field("tool order", &self.tool_order)
            .field("sections", &self.sections.len())
            .field("contexts", &self.contexts.len())
            .field("variables", &self.variables.len())
            .field("tool providers", &self.tool_providers.len())
            .field("next tool provider", &self.next_tool_provider)
            .field("suppressions", &self.suppressions.len())
            .field("next suppression", &self.next_suppression)
            .field("listeners", &self.waterfall.len())
            .finish()
    }
}

/// Code-unit name comparison (dsh `compareNames`).
fn compare_names(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Order sections by explicit placement, then deterministically by name
/// (dsh `comparePromptSections`).
fn compare_prompt_sections(a: &PromptSectionSpec, b: &PromptSectionSpec) -> Ordering {
    a.order
        .total_cmp(&b.order)
        .then_with(|| compare_names(&a.name, &b.name))
}

/// Resolve one registered text against the resolver seam.
fn resolve_text(
    text: &SectionText,
    context: &WireAssembleContext,
    resolver: &mut dyn ProviderResolver,
) -> Result<String, PromptError> {
    match text {
        SectionText::Literal(text) => Ok(text.clone()),
        SectionText::Endpoint { text_provider } => resolver
            .text(text_provider, context)
            .map_err(PromptError::new),
    }
}

/// Resolve one registered context into its assembled form (dsh
/// `resolveContext`).
fn resolve_context(
    spec: &PromptContextSpec,
    context: &WireAssembleContext,
    resolver: &mut dyn ProviderResolver,
) -> Result<AssembledContext, PromptError> {
    Ok(AssembledContext {
        name: spec.name.clone(),
        text: resolve_text(&spec.text, context, resolver)?,
        interpolate: spec.interpolate,
    })
}

impl SystemPromptRegistry {
    /// Builds the registry and registers the harness-owned openers (dsh
    /// constructor).
    ///
    /// # Panics
    ///
    /// Panics when a builtin placement is missing from the generated
    /// order table, which is a build-time inconsistency.
    ///
    /// # Errors
    ///
    /// Returns the verbatim `toolOrder` validation failure.
    pub fn new(config: &RegistryConfig) -> Result<Self, PromptError> {
        let tool_order = match &config.tool_order {
            Some(order) => {
                validate_tool_order(order)?;
                Some(order.clone())
            }
            None => None,
        };
        let mut registry = Self {
            include_runtime_context: config.include_runtime_context.unwrap_or(true),
            persona_prefix: config.persona_prefix.clone().unwrap_or_default(),
            persona_suffix: config.persona_suffix.clone().unwrap_or_default(),
            tool_order,
            ..Self::default()
        };
        // Keep harness-owned openers independent of the selected loop
        // plugin (dsh `includeHarnessIdentity ?? true`).
        if config.include_harness_identity.unwrap_or(true) {
            registry.sections.insert(
                HARNESS_IDENTITY_SECTION.to_string(),
                PromptSectionSpec {
                    name: HARNESS_IDENTITY_SECTION.to_string(),
                    order: order_value("HARNESS_IDENTITY"),
                    text: SectionText::Literal(HARNESS_IDENTITY_TEXT.to_string()),
                    interpolate: None,
                    complete: None,
                },
            );
        }
        for (name, order_name, text) in [
            (
                PERSONA_PREFIX_SECTION,
                "DEPLOYMENT_PERSONA_PREFIX",
                registry.persona_prefix.clone(),
            ),
            (
                PERSONA_SUFFIX_SECTION,
                "DEPLOYMENT_PERSONA_SUFFIX",
                registry.persona_suffix.clone(),
            ),
        ] {
            registry.sections.insert(
                name.to_string(),
                PromptSectionSpec {
                    name: name.to_string(),
                    order: order_value(order_name),
                    text: SectionText::Literal(text),
                    interpolate: None,
                    complete: None,
                },
            );
        }
        if !registry.include_runtime_context {
            registry.suppress_runtime_context();
        }
        Ok(registry)
    }

    /// Resolves a centrally owned section placement (dsh
    /// `getSectionOrder`).
    #[must_use]
    pub fn get_section_order(name: &str) -> Option<i64> {
        section_order(name)
    }

    /// Resolves a centrally owned context placement (dsh
    /// `getContextOrder`).
    #[must_use]
    pub fn get_context_order(name: &str) -> Option<i64> {
        context_order(name)
    }

    /// Registers one ordered prompt section (dsh `section`).
    ///
    /// # Errors
    ///
    /// Returns the verbatim duplicate-name or non-finite-order failure.
    pub fn register_section(&mut self, spec: PromptSectionSpec) -> Result<(), PromptError> {
        if !spec.order.is_finite() {
            return Err(PromptError::new(format!(
                "prompt section \"{}\" order must be a finite number",
                spec.name
            )));
        }
        if self.sections.contains_key(&spec.name) {
            return Err(PromptError::new(format!(
                "prompt section \"{}\" is already registered{DUPLICATE_HINT}",
                spec.name
            )));
        }
        self.sections.insert(spec.name.clone(), spec);
        Ok(())
    }

    /// Removes one section registration; an absent name is a no-op (the
    /// dsh disposer is idempotent).
    pub fn unregister_section(&mut self, name: &str) {
        self.sections.shift_remove(name);
    }

    /// Registers one ordered dynamic context (dsh `context`).
    ///
    /// # Errors
    ///
    /// Returns the verbatim duplicate-name or non-finite-order failure.
    pub fn register_context(&mut self, spec: PromptContextSpec) -> Result<(), PromptError> {
        if !spec.order.is_finite() {
            return Err(PromptError::new(format!(
                "prompt context \"{}\" order must be a finite number",
                spec.name
            )));
        }
        if self.contexts.contains_key(&spec.name) {
            return Err(PromptError::new(format!(
                "prompt context \"{}\" is already registered{DUPLICATE_HINT}",
                spec.name
            )));
        }
        self.contexts.insert(spec.name.clone(), spec);
        Ok(())
    }

    /// Removes one context registration; an absent name is a no-op.
    pub fn unregister_context(&mut self, name: &str) {
        self.contexts.shift_remove(name);
    }

    /// Registers one prompt variable (dsh `variable`).
    ///
    /// # Errors
    ///
    /// Returns the verbatim invalid-name or duplicate-name failure.
    pub fn register_variable(
        &mut self,
        name: &str,
        source: VariableSource,
    ) -> Result<(), PromptError> {
        if !valid_variable_name(name) {
            return Err(PromptError::new(format!(
                "invalid prompt variable name \"{name}\" (must match /^[a-z][a-z0-9_]*$/)"
            )));
        }
        if self.variables.contains_key(name) {
            return Err(PromptError::new(format!(
                "prompt variable \"{name}\" is already registered{DUPLICATE_HINT}"
            )));
        }
        self.variables.insert(name.to_string(), source);
        Ok(())
    }

    /// Removes one variable registration; an absent name is a no-op.
    pub fn unregister_variable(&mut self, name: &str) {
        self.variables.shift_remove(name);
    }

    /// Registers one tool-schema provider (dsh `tools`); the token
    /// unregisters it.
    pub fn register_tools(&mut self, registration: ToolProviderRegistration) -> ToolProviderToken {
        let token = ToolProviderToken(self.next_tool_provider);
        self.next_tool_provider += 1;
        self.tool_providers.insert(token.0, registration);
        token
    }

    /// Removes one tool-provider registration; an absent token is a no-op.
    pub fn unregister_tools(&mut self, token: ToolProviderToken) {
        self.tool_providers.shift_remove(&token.0);
    }

    /// Suppresses optional dynamic runtime-context contributions (dsh
    /// `suppressRuntimeContext`); the token releases it.
    pub fn suppress_runtime_context(&mut self) -> SuppressionToken {
        let token = SuppressionToken(self.next_suppression);
        self.next_suppression += 1;
        self.suppressions.push(token.0);
        token
    }

    /// Releases one suppression; an absent token is a no-op.
    pub fn release_suppression(&mut self, token: SuppressionToken) {
        self.suppressions.retain(|id| *id != token.0);
    }

    /// Registers one `system-prompt/assemble` waterfall listener (Q87-A).
    pub fn on_assemble(&mut self, listener: AssembleListener) {
        self.waterfall.push(listener);
    }

    /// Whether optional runtime context is currently suppressed.
    #[must_use]
    pub const fn runtime_context_suppressed(&self) -> bool {
        !self.suppressions.is_empty()
    }

    /// Assembles providers into one model input, applies canonical
    /// ordering, runs the waterfall, then enforces any complete section
    /// and current suppression (dsh `assemble`).
    ///
    /// # Errors
    ///
    /// Returns the first provider, ordering, or complete-section failure.
    pub fn assemble(
        &self,
        context: &WireAssembleContext,
        resolver: &mut dyn ProviderResolver,
    ) -> Result<PromptAssembly, PromptError> {
        let suppressed = self.runtime_context_suppressed();
        // Variables resolve in registration order.
        let mut variables: Variables = IndexMap::new();
        for (name, source) in &self.variables {
            let value = match source {
                VariableSource::Literal { value } => Some(value.clone()),
                VariableSource::Endpoint { value_provider } => resolver
                    .variable(value_provider, context)
                    .map_err(PromptError::new)?,
            };
            variables.insert(name.clone(), value);
        }
        let mut section_definitions: Vec<&PromptSectionSpec> = self.sections.values().collect();
        section_definitions.sort_unstable_by(|a, b| compare_prompt_sections(a, b));
        let complete_names: Vec<&str> = section_definitions
            .iter()
            .filter(|section| section.complete == Some(true))
            .map(|section| section.name.as_str())
            .collect();
        if complete_names.len() > 1 {
            let listed = complete_names
                .iter()
                .map(|name| serde_json::to_string(name).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(PromptError::new(format!(
                "multiple complete prompt sections are active: {listed}"
            )));
        }
        let mut complete_section: Option<AssembledSection> = None;
        let mut sections = Vec::with_capacity(section_definitions.len());
        for section in section_definitions {
            let assembled = AssembledSection {
                name: section.name.clone(),
                text: resolve_text(&section.text, context, resolver)?,
                interpolate: section.interpolate,
            };
            if section.complete == Some(true) {
                complete_section = Some(assembled.clone());
            }
            sections.push(assembled);
        }
        let mut ordered_contexts: Vec<&PromptContextSpec> = self.contexts.values().collect();
        ordered_contexts.sort_by(|a, b| a.order.total_cmp(&b.order));
        let contexts: Vec<AssembledContext> = ordered_contexts
            .iter()
            .filter(|spec| !suppressed || spec.required == Some(true))
            .map(|spec| resolve_context(spec, context, resolver))
            .collect::<Result<_, _>>()?;
        // Collect visible schemas and the pre-restriction name universe.
        let mut collected = Vec::new();
        let mut known_names = HashSet::new();
        for registration in self.tool_providers.values() {
            let result = resolver
                .tools(&registration.provider, context)
                .map_err(PromptError::new)?;
            let known = result.known_names.clone().unwrap_or_else(|| {
                result
                    .schemas
                    .iter()
                    .map(|schema| schema.name.clone())
                    .collect()
            });
            collected.extend(result.schemas);
            for name in known {
                known_names.insert(name);
            }
        }
        let tools = order_tools(&collected, self.tool_order.as_deref(), &known_names)?;
        let assembly = PromptAssembly {
            sections,
            contexts,
            tools,
            variables,
        };
        let listeners = self.waterfall.clone();
        let transformed = run_waterfall(&listeners, assembly, context, |payload| {
            Ok::<PromptAssembly, String>(payload)
        })?;
        if complete_section.is_none() && !suppressed {
            return Ok(transformed);
        }
        let required: HashSet<&str> = self
            .contexts
            .values()
            .filter(|spec| spec.required == Some(true))
            .map(|spec| spec.name.as_str())
            .collect();
        Ok(PromptAssembly {
            sections: complete_section
                .map_or_else(|| transformed.sections, |section| vec![section]),
            contexts: if suppressed {
                transformed
                    .contexts
                    .into_iter()
                    .filter(|context| required.contains(context.name.as_str()))
                    .collect()
            } else {
                transformed.contexts
            },
            ..transformed
        })
    }

    /// Refreshes accepted runtime facts for request admission without
    /// reassembling sections, tools, or variables (dsh `refreshContext`).
    ///
    /// # Panics
    ///
    /// Panics when a required entry leaves the registry between the
    /// lookup and its resolution, which cannot happen under `&self`.
    ///
    /// # Errors
    ///
    /// Returns the first context provider failure.
    pub fn refresh_context(
        &self,
        assembly: &PromptAssembly,
        context: &WireAssembleContext,
        resolver: &mut dyn ProviderResolver,
    ) -> Result<PromptAssembly, PromptError> {
        let suppressed = self.runtime_context_suppressed();
        let mut required: IndexMap<&str, &PromptContextSpec> = self
            .contexts
            .iter()
            .filter(|(_, spec)| spec.required == Some(true))
            .map(|(name, spec)| (name.as_str(), spec))
            .collect();
        let mut contexts: Vec<AssembledContext> = Vec::new();
        for accepted in &assembly.contexts {
            required.shift_remove(accepted.name.as_str());
            let provider = self.contexts.get(&accepted.name);
            if suppressed && provider.map(|spec| spec.required) != Some(Some(true)) {
                continue;
            }
            contexts.push(match provider {
                None => accepted.clone(),
                Some(spec) => resolve_context(spec, context, resolver)?,
            });
        }
        for (name, spec) in required {
            // Insert before the first entry whose own provider sorts
            // later; otherwise append (dsh splice semantics).
            let index = contexts.iter().position(|entry| {
                self.contexts
                    .get(&entry.name)
                    .is_some_and(|next| next.order > spec.order)
            });
            let refreshed = resolve_context(
                self.contexts
                    .get(name)
                    .expect("required entries come from the registry"),
                context,
                resolver,
            )?;
            match index {
                Some(at) => contexts.insert(at, refreshed),
                None => contexts.push(refreshed),
            }
        }
        Ok(PromptAssembly {
            contexts,
            ..assembly.clone()
        })
    }
}

/// Runs the in-process assemble waterfall: the first listener is the
/// outermost frame (dsh dispatch order, Q87-A).
fn run_waterfall(
    listeners: &[AssembleListener],
    assembly: PromptAssembly,
    context: &WireAssembleContext,
    tail: impl FnOnce(PromptAssembly) -> Result<PromptAssembly, String>,
) -> Result<PromptAssembly, PromptError> {
    let Some((first, rest)) = listeners.split_first() else {
        return tail(assembly).map_err(PromptError::new);
    };
    let inner = Box::new(move |payload| {
        run_waterfall(rest, payload, context, tail).map_err(|error| error.0)
    });
    first(assembly, context, inner).map_err(PromptError::new)
}

/// A builtin placement as a finite section order (the generated tables
/// are integral and far below 2^52, so the widening cast is exact).
#[expect(
    clippy::cast_precision_loss,
    reason = "the generated order tables are small integers, exact in f64"
)]
fn order_value(name: &str) -> f64 {
    section_order(name).expect("the generated table names every builtin placement") as f64
}

/// Looks one placement up in the generated section table.
fn section_order(name: &str) -> Option<i64> {
    SECTION_ORDERS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| *value)
}

/// Looks one placement up in the generated context table.
fn context_order(name: &str) -> Option<i64> {
    CONTEXT_ORDERS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| *value)
}

/// `^[a-z][a-z0-9_]*$`.
fn valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::interpolate::render_prompt;

    fn endpoint(service: &str, method: u32) -> EndpointRef {
        EndpointRef {
            service: service.to_string(),
            method,
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "test placements are small integers, exact in f64"
    )]
    fn literal_section(name: &str, order: i64, text: &str) -> PromptSectionSpec {
        PromptSectionSpec {
            name: name.to_string(),
            order: order as f64,
            text: SectionText::Literal(text.to_string()),
            interpolate: None,
            complete: None,
        }
    }

    /// Drives every endpoint from lookup tables and records nothing else.
    struct FakeResolver {
        texts: HashMap<EndpointRef, String>,
        variables: HashMap<EndpointRef, Option<String>>,
        tools: HashMap<EndpointRef, ToolProviderResult>,
    }

    impl ProviderResolver for FakeResolver {
        fn text(
            &mut self,
            endpoint: &EndpointRef,
            _context: &WireAssembleContext,
        ) -> Result<String, String> {
            self.texts
                .get(endpoint)
                .cloned()
                .ok_or_else(|| format!("no text fake for {endpoint:?}"))
        }

        fn variable(
            &mut self,
            endpoint: &EndpointRef,
            _context: &WireAssembleContext,
        ) -> Result<Option<String>, String> {
            self.variables
                .get(endpoint)
                .cloned()
                .ok_or_else(|| format!("no variable fake for {endpoint:?}"))
        }

        fn tools(
            &mut self,
            endpoint: &EndpointRef,
            _context: &WireAssembleContext,
        ) -> Result<ToolProviderResult, String> {
            self.tools
                .get(endpoint)
                .cloned()
                .ok_or_else(|| format!("no tools fake for {endpoint:?}"))
        }
    }

    fn blank_resolver() -> FakeResolver {
        FakeResolver {
            texts: HashMap::new(),
            variables: HashMap::new(),
            tools: HashMap::new(),
        }
    }

    #[test]
    fn constructor_registers_identity_and_both_personas() {
        let registry =
            SystemPromptRegistry::new(&RegistryConfig::default()).expect("default config");
        let mut resolver = blank_resolver();
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        let names: Vec<&str> = assembly.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "harness:identity",
                "deployment:persona-prefix",
                "deployment:persona-suffix"
            ]
        );
        assert_eq!(assembly.sections[0].text, HARNESS_IDENTITY_TEXT);
        assert_eq!(
            SystemPromptRegistry::get_section_order("HARNESS_IDENTITY"),
            Some(-1000)
        );
    }

    #[test]
    fn constructor_omits_identity_when_disabled() {
        let registry = SystemPromptRegistry::new(&RegistryConfig {
            include_harness_identity: Some(false),
            persona_prefix: Some("custom persona".to_string()),
            ..RegistryConfig::default()
        })
        .expect("config");
        let mut resolver = blank_resolver();
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        assert_eq!(assembly.sections.len(), 2, "persona slots only");
        assert_eq!(assembly.sections[0].text, "custom persona");
    }

    #[test]
    fn constructor_suppresses_runtime_context_when_disabled() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig {
            include_runtime_context: Some(false),
            ..RegistryConfig::default()
        })
        .expect("config");
        registry
            .register_context(PromptContextSpec {
                name: "optional".to_string(),
                order: 100.0,
                text: SectionText::Literal("cwd".to_string()),
                interpolate: None,
                required: None,
            })
            .expect("register");
        registry
            .register_context(PromptContextSpec {
                name: "must".to_string(),
                order: 110.0,
                text: SectionText::Literal("policy".to_string()),
                interpolate: None,
                required: Some(true),
            })
            .expect("register");
        let mut resolver = blank_resolver();
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        let names: Vec<&str> = assembly.contexts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["must"], "required context survives suppression");
    }

    #[test]
    fn duplicate_section_name_throws_with_the_ctx_hint() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        registry
            .register_section(literal_section("s", 10, "one"))
            .expect("first");
        let error = registry
            .register_section(literal_section("s", 20, "two"))
            .unwrap_err();
        assert_eq!(
            error.0,
            "prompt section \"s\" is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)"
        );
    }

    #[test]
    fn non_finite_orders_throw() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        let mut section = literal_section("s", 1, "t");
        section.order = f64::NAN;
        assert_eq!(
            registry.register_section(section).unwrap_err().0,
            "prompt section \"s\" order must be a finite number"
        );
        let context = PromptContextSpec {
            name: "c".to_string(),
            order: f64::INFINITY,
            text: SectionText::Literal(String::new()),
            interpolate: None,
            required: None,
        };
        assert_eq!(
            registry.register_context(context).unwrap_err().0,
            "prompt context \"c\" order must be a finite number"
        );
    }

    #[test]
    fn sections_sort_by_order_then_code_unit_name() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        for (name, order) in [("beta", 10), ("alpha", 10), ("late", 900)] {
            registry
                .register_section(literal_section(name, order, name))
                .expect("register");
        }
        let mut resolver = blank_resolver();
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        let names: Vec<String> = assembly
            .sections
            .iter()
            .map(|s| s.name.clone())
            .filter(|name| name != "harness:identity" && !name.starts_with("deployment:"))
            .collect();
        assert_eq!(names, ["alpha", "beta", "late"]);
    }

    #[test]
    fn complete_conflict_names_both_sections() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        for name in ["a", "b"] {
            let mut section = literal_section(name, 10, name);
            section.complete = Some(true);
            registry.register_section(section).expect("register");
        }
        let mut resolver = blank_resolver();
        let error = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .unwrap_err();
        assert_eq!(
            error.0,
            "multiple complete prompt sections are active: \"a\", \"b\""
        );
    }

    #[test]
    fn complete_section_restored_after_the_waterfall() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        let mut complete = literal_section("whole", 10, "the whole prompt");
        complete.complete = Some(true);
        registry.register_section(complete).expect("register");
        registry
            .register_section(literal_section("extra", 20, "extra"))
            .expect("register");
        registry.on_assemble(Arc::new(|mut assembly, _context, next| {
            // The listener replaces the sections wholesale; the registry
            // still restores the complete section afterwards.
            assembly.sections.push(AssembledSection {
                name: "listener".to_string(),
                text: String::new(),
                interpolate: None,
            });
            next(assembly)
        }));
        let mut resolver = blank_resolver();
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        assert_eq!(assembly.sections.len(), 1);
        assert_eq!(assembly.sections[0].name, "whole");
        assert_eq!(assembly.sections[0].text, "the whole prompt");
    }

    #[test]
    fn waterfall_listener_can_veto_the_chain() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        registry.on_assemble(Arc::new(|assembly, _context, _next| {
            // Dropping `next` vetoes the rest of the chain.
            Ok(PromptAssembly {
                sections: vec![AssembledSection {
                    name: "veto".to_string(),
                    text: "kept".to_string(),
                    interpolate: None,
                }],
                ..assembly
            })
        }));
        registry.on_assemble(Arc::new(|assembly, _context, next| {
            let mut payload = assembly;
            payload.sections.push(AssembledSection {
                name: "second".to_string(),
                text: String::new(),
                interpolate: None,
            });
            next(payload)
        }));
        let mut resolver = blank_resolver();
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        let names: Vec<&str> = assembly.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["veto"]);
    }

    #[test]
    fn endpoints_resolve_text_variables_and_tools() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        registry
            .register_section(PromptSectionSpec {
                name: "endpoint".to_string(),
                order: 5.0,
                text: SectionText::Endpoint {
                    text_provider: endpoint("tools", 4),
                },
                interpolate: None,
                complete: None,
            })
            .expect("register");
        registry
            .register_variable(
                "cwd",
                VariableSource::Endpoint {
                    value_provider: endpoint("env", 2),
                },
            )
            .expect("register");
        registry.register_tools(ToolProviderRegistration {
            provider: endpoint("tools", 6),
        });
        let mut resolver = FakeResolver {
            texts: HashMap::from([(endpoint("tools", 4), "working at {{cwd}}".to_string())]),
            variables: HashMap::from([(endpoint("env", 2), Some("/w".to_string()))]),
            tools: HashMap::from([(
                endpoint("tools", 6),
                ToolProviderResult {
                    schemas: vec![fr_wire::call_config::ToolSchema {
                        defer_loading: None,
                        name: "bash".to_string(),
                        description: "shell".to_string(),
                        parameters: serde_json::Map::new(),
                    }],
                    known_names: Some(vec!["bash".to_string(), "hidden".to_string()]),
                },
            )]),
        };
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        assert_eq!(assembly.variables.get("cwd"), Some(&Some("/w".to_string())));
        assert_eq!(assembly.tools.len(), 1);
        assert_eq!(assembly.tools[0].name, "bash");
        let rendered = render_prompt(&assembly).expect("render");
        assert!(rendered.contains("working at /w"), "{rendered}");
    }

    #[test]
    fn configured_tool_order_applies_to_collected_schemas() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig {
            tool_order: Some(vec!["z".to_string(), "<unlisted-tools>".to_string()]),
            ..RegistryConfig::default()
        })
        .expect("config");
        let schema = |name: &str| fr_wire::call_config::ToolSchema {
            defer_loading: None,
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::Map::new(),
        };
        registry.register_tools(ToolProviderRegistration {
            provider: endpoint("tools", 6),
        });
        let mut resolver = FakeResolver {
            texts: HashMap::new(),
            variables: HashMap::new(),
            tools: HashMap::from([(
                endpoint("tools", 6),
                ToolProviderResult {
                    schemas: vec![schema("a"), schema("z")],
                    known_names: None,
                },
            )]),
        };
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        let names: Vec<&str> = assembly.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["z", "a"], "rest entry inserts unlisted tools");
    }

    #[test]
    fn invalid_tool_order_fails_construction() {
        let error = SystemPromptRegistry::new(&RegistryConfig {
            tool_order: Some(vec!["only".to_string()]),
            ..RegistryConfig::default()
        })
        .unwrap_err();
        assert_eq!(
            error.0,
            "toolOrder must contain the \"<unlisted-tools>\" rest entry (where unlisted tools are inserted)"
        );
    }

    #[test]
    fn refresh_reruns_contexts_and_keeps_accepted_sections() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        registry
            .register_section(literal_section("stable", 10, "section text"))
            .expect("register");
        registry
            .register_context(PromptContextSpec {
                name: "volatile".to_string(),
                order: 100.0,
                text: SectionText::Endpoint {
                    text_provider: endpoint("env", 2),
                },
                interpolate: None,
                required: None,
            })
            .expect("register");
        let mut resolver = FakeResolver {
            texts: HashMap::from([(endpoint("env", 2), "first".to_string())]),
            variables: HashMap::new(),
            tools: HashMap::new(),
        };
        let accepted = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        assert_eq!(accepted.contexts[0].text, "first");
        resolver
            .texts
            .insert(endpoint("env", 2), "second".to_string());
        let refreshed = registry
            .refresh_context(&accepted, &WireAssembleContext::default(), &mut resolver)
            .expect("refresh");
        assert_eq!(refreshed.contexts[0].text, "second");
        assert_eq!(refreshed.sections, accepted.sections);
        assert_eq!(refreshed.tools, accepted.tools);
    }

    #[test]
    fn refresh_restores_missing_required_contexts_in_order() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        registry
            .register_context(PromptContextSpec {
                name: "later".to_string(),
                order: 200.0,
                text: SectionText::Literal("later".to_string()),
                interpolate: None,
                required: Some(true),
            })
            .expect("register");
        registry
            .register_context(PromptContextSpec {
                name: "missing".to_string(),
                order: 150.0,
                text: SectionText::Literal("middle".to_string()),
                interpolate: None,
                required: Some(true),
            })
            .expect("register");
        // An accepted assembly that predates the "missing" registration.
        let accepted = PromptAssembly {
            sections: Vec::new(),
            contexts: vec![AssembledContext {
                name: "later".to_string(),
                text: "stale".to_string(),
                interpolate: None,
            }],
            tools: Vec::new(),
            variables: IndexMap::new(),
        };
        let mut resolver = blank_resolver();
        let refreshed = registry
            .refresh_context(&accepted, &WireAssembleContext::default(), &mut resolver)
            .expect("refresh");
        let names: Vec<(&str, &str)> = refreshed
            .contexts
            .iter()
            .map(|c| (c.name.as_str(), c.text.as_str()))
            .collect();
        assert_eq!(names, [("missing", "middle"), ("later", "later")]);
    }

    #[test]
    fn refresh_keeps_waterfall_additions_without_providers() {
        let registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        let accepted = PromptAssembly {
            sections: Vec::new(),
            contexts: vec![AssembledContext {
                name: "expert".to_string(),
                text: "from listener".to_string(),
                interpolate: None,
            }],
            tools: Vec::new(),
            variables: IndexMap::new(),
        };
        let mut resolver = blank_resolver();
        let refreshed = registry
            .refresh_context(&accepted, &WireAssembleContext::default(), &mut resolver)
            .expect("refresh");
        assert_eq!(refreshed.contexts, accepted.contexts);
    }

    #[test]
    fn suppression_release_restores_optional_contexts() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        registry
            .register_context(PromptContextSpec {
                name: "optional".to_string(),
                order: 100.0,
                text: SectionText::Literal("cwd".to_string()),
                interpolate: None,
                required: None,
            })
            .expect("register");
        let token = registry.suppress_runtime_context();
        let mut resolver = blank_resolver();
        let suppressed = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        assert_eq!(suppressed.contexts, Vec::new());
        registry.release_suppression(token);
        let restored = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        assert_eq!(restored.contexts.len(), 1);
    }

    #[test]
    fn variable_registration_validates_and_rejects_duplicates() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        let error = registry
            .register_variable(
                "BadName",
                VariableSource::Literal {
                    value: String::new(),
                },
            )
            .unwrap_err();
        assert_eq!(
            error.0,
            "invalid prompt variable name \"BadName\" (must match /^[a-z][a-z0-9_]*$/)"
        );
        registry
            .register_variable(
                "good",
                VariableSource::Literal {
                    value: "v".to_string(),
                },
            )
            .expect("register");
        let error = registry
            .register_variable(
                "good",
                VariableSource::Literal {
                    value: "w".to_string(),
                },
            )
            .unwrap_err();
        assert_eq!(
            error.0,
            "prompt variable \"good\" is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)"
        );
    }

    #[test]
    fn unregister_removes_contributions() {
        let mut registry = SystemPromptRegistry::new(&RegistryConfig::default()).expect("config");
        registry
            .register_section(literal_section("gone", 10, "text"))
            .expect("register");
        registry.unregister_section("gone");
        // Unregistering an absent name stays a no-op.
        registry.unregister_section("gone");
        let mut resolver = blank_resolver();
        let assembly = registry
            .assemble(&WireAssembleContext::default(), &mut resolver)
            .expect("assemble");
        assert!(!assembly.sections.iter().any(|s| s.name == "gone"));
    }
}
