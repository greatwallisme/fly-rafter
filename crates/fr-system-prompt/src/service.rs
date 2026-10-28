// Rust guideline compliant 2026-10-28
//! The `system-prompt` service face (Q86-A): assembly, refresh, the
//! registration family, and the workspace-instruction compose (Q88-A)
//! over the dougong dynamic channel.
//!
//! Method 2-3 serve the loop's step boundary; 4-8 are the registration
//! family (dynamic providers register as callback endpoints the service
//! resolves through its copied require vtable at each assembly); 9 is the
//! agent-instructions compose (dsh agent-instructions/src/index.ts
//! `compose`, verbatim flow). Filesystem access rides the seam; compose
//! runs inline on the calling thread (the fr-session append precedent:
//! bounded local I/O on the synchronous service path).

use std::collections::HashSet;

use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_OK};

use crate::instructions::config::{InstructionConfig, ResolvedInstructionConfig, resolve_config};
use crate::instructions::files::{FileSeam, find_project_root, load_baseline_instruction_set};
use crate::instructions::render::InstructionChangeAction;
use crate::instructions::state::{
    ReconcileInput, VisibleMessage, apply_instruction_version_updates, baseline_instruction_state,
    reconcile_instruction_context,
};
use crate::interpolate::PromptAssembly;
use crate::registry::{
    EndpointRef, PromptContextSpec, PromptSectionSpec, ProviderResolver, RegistryConfig,
    SystemPromptRegistry, ToolProviderRegistration, ToolProviderResult, VariableSource,
    WireAssembleContext,
};

/// The provided-service name consumers inject (PRD :336).
pub const SERVICE_NAME: &str = "system-prompt";

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 1;
/// Method selector: assemble one model input.
pub const METHOD_ASSEMBLE: u32 = 2;
/// Method selector: refresh volatile context sections.
pub const METHOD_REFRESH_CONTEXT: u32 = 3;
/// Method selector: register one prompt section.
pub const METHOD_REGISTER_SECTION: u32 = 4;
/// Method selector: register one dynamic context.
pub const METHOD_REGISTER_CONTEXT: u32 = 5;
/// Method selector: register one prompt variable.
pub const METHOD_REGISTER_VARIABLE: u32 = 6;
/// Method selector: register one tool-schema provider.
pub const METHOD_REGISTER_TOOLS: u32 = 7;
/// Method selector: remove one registration.
pub const METHOD_UNREGISTER: u32 = 8;
/// Method selector: compose the workspace-instruction message.
pub const METHOD_COMPOSE_INSTRUCTIONS: u32 = 9;

/// The host protocol's documented out-buffer ceiling (16 MiB).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// One copied require vtable (the outbound.rs judgment, reduced to the
/// `require` face this service's callback endpoints need).
#[derive(Debug, Clone, Copy)]
pub struct RequireSeam {
    host: HostExports,
}

// SAFETY: the ABI vtable contract allows `require` from any plugin
// thread, concurrently; the copy stays valid until this activation's
// `teardown` returns, and this service never outlives its activation.
unsafe impl Send for RequireSeam {}

// SAFETY: sharing one copy only issues concurrent vtable calls, which the
// same contract explicitly allows.
unsafe impl Sync for RequireSeam {}

impl RequireSeam {
    /// Wraps a vtable copied at `setup`.
    #[must_use]
    pub const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Performs one synchronous cross-plugin call (growable buffer
    /// protocol, `STATUS_CAPACITY` doubles up to the 16 MiB ceiling).
    fn require(&self, service: &str, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: the copied vtable is live until this activation's
            // `teardown` returns; buffers are caller-owned per the
            // export-table contract.
            let status = unsafe {
                (self.host.require)(
                    self.host.host_handle,
                    service.as_ptr(),
                    service.len(),
                    method,
                    payload.as_ptr(),
                    payload.len(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut len,
                )
            };
            if status == STATUS_CAPACITY {
                if cap >= MAX_REPLY_CAPACITY {
                    return Err(format!(
                        "endpoint reply still exceeds {MAX_REPLY_CAPACITY} bytes"
                    ));
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            return Err(format!(
                "endpoint require {service}.{method} failed with host status {status}: {}",
                String::from_utf8_lossy(&buf)
            ));
        }
    }
}

impl ProviderResolver for RequireSeam {
    fn text(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<String, String> {
        let payload = serde_json::to_vec(context).map_err(|error| error.to_string())?;
        let reply = self.require(&endpoint.service, endpoint.method, &payload)?;
        let text: String = serde_json::from_slice(&reply)
            .map_err(|error| format!("text endpoint reply is not a string: {error}"))?;
        Ok(text)
    }

    fn variable(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<Option<String>, String> {
        let payload = serde_json::to_vec(context).map_err(|error| error.to_string())?;
        let reply = self.require(&endpoint.service, endpoint.method, &payload)?;
        if reply.is_empty() {
            return Ok(None);
        }
        let value: Option<String> = serde_json::from_slice(&reply)
            .map_err(|error| format!("variable endpoint reply is not a value: {error}"))?;
        Ok(value)
    }

    fn tools(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<ToolProviderResult, String> {
        let payload = serde_json::to_vec(context).map_err(|error| error.to_string())?;
        let reply = self.require(&endpoint.service, endpoint.method, &payload)?;
        serde_json::from_slice(&reply)
            .map_err(|error| format!("tools endpoint reply is not a provider result: {error}"))
    }
}

/// One failed service call (the `{"error": ...}` `STATUS_FAILED` body).
#[derive(Debug)]
pub struct ServiceError(pub String);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceError {}

/// The `refresh_context` request payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshRequest {
    /// The accepted assembly for this scope and step.
    pub assembly: PromptAssembly,
    /// The current assembly context.
    #[serde(default)]
    pub context: WireAssembleContext,
}

/// The `register_variable` request payload (static value or endpoint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RegisterVariableRequest {
    /// A static value.
    Literal {
        /// The variable's reference name.
        name: String,
        /// The static value.
        value: String,
    },
    /// A callback endpoint.
    #[serde(rename_all = "camelCase")]
    Endpoint {
        /// The variable's reference name.
        name: String,
        /// The provider endpoint.
        value_provider: EndpointRef,
    },
}

/// The `unregister` request payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnregisterRequest {
    /// Which registration family the target belongs to.
    pub kind: UnregisterKind,
    /// The registered name, for the name-keyed families.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The disposer token, for the token-keyed families.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<u64>,
}

/// The registration families `unregister` removes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnregisterKind {
    /// A prompt section.
    Section,
    /// A dynamic context.
    Context,
    /// A prompt variable.
    Variable,
    /// A tool-schema provider.
    Tools,
    /// A runtime-context suppression.
    Suppression,
}

/// The `compose_instructions` request payload (the loop bridge builds it
/// from the claimed batch, the pending projection, the touched paths, and
/// the session surface).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposeRequest {
    /// The bound session id (per-session compose state key).
    pub session_id: String,
    /// The session working directory.
    pub cwd: String,
    /// The claimed batch's instruction-relevant messages.
    #[serde(default)]
    pub claimed: Vec<VisibleMessage>,
    /// The pending projection's messages (next-step order).
    #[serde(default)]
    pub pending: Vec<VisibleMessage>,
    /// Paths the fs tools touched since the last compose.
    #[serde(default)]
    pub touched_paths: Vec<String>,
    /// The session surface's instruction-relevant messages, surface order.
    #[serde(default)]
    pub surface: Vec<VisibleMessage>,
}

impl ComposeRequest {
    /// Builds a minimal request for tests.
    #[must_use]
    pub fn new(session_id: &str, cwd: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            claimed: Vec::new(),
            pending: Vec::new(),
            touched_paths: Vec::new(),
            surface: Vec::new(),
        }
    }
}

/// The `compose_instructions` reply: the desired user message or none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposeReply {
    /// The composed message, when one is due.
    pub message: Option<InstructionMessage>,
}

/// One composed workspace-instruction message in the wire shape the loop
/// bridge lowers to a user message (dsh `createUserMessage` payload).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "the source map embeds serde_json values (f64), so Eq is not derivable"
)]
#[serde(rename_all = "camelCase")]
pub struct InstructionMessage {
    /// The text blocks.
    pub content: Vec<InstructionTextBlock>,
    /// The agent-instructions source document.
    pub source: serde_json::Map<String, serde_json::Value>,
}

/// One text block of a composed message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionTextBlock {
    /// The block type.
    #[serde(rename = "type")]
    pub block_type: String,
    /// The block text.
    pub text: String,
}

/// Per-session compose state (dsh `instructionVersions` +
/// `baselinePreparations`).
#[derive(Debug, Default)]
struct SessionState {
    versions: crate::instructions::state::VersionCache,
    preparation: Option<Preparation>,
}

/// One remembered baseline preparation (dsh `baselinePreparations`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Preparation {
    identity: String,
    excluded_scopes: HashSet<String>,
}

/// The service state: the registry, the compose configuration, per-session
/// state, the filesystem seam, and the copied require vtable.
pub struct PromptService {
    registry: SystemPromptRegistry,
    compose_config: ResolvedInstructionConfig,
    sessions: std::collections::HashMap<String, SessionState>,
    seam: Box<dyn FileSeam>,
    require: Option<RequireSeam>,
}

impl std::fmt::Debug for PromptService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptService")
            .field("registry", &self.registry)
            .field("compose config", &self.compose_config)
            .field("sessions", &self.sessions.len())
            .field("seam", &"dyn FileSeam")
            .field("require seam", &self.require.is_some())
            .finish()
    }
}

impl PromptService {
    /// Builds the service over one registry configuration, one compose
    /// configuration, and a filesystem seam.
    ///
    /// # Errors
    ///
    /// Returns the registry construction failure (invalid `toolOrder`).
    pub fn new(
        registry_config: &RegistryConfig,
        instruction_config: &InstructionConfig,
        seam: Box<dyn FileSeam>,
    ) -> Result<Self, crate::interpolate::PromptError> {
        let no_env = |_: &str| None::<String>;
        Ok(Self {
            registry: SystemPromptRegistry::new(registry_config)?,
            compose_config: resolve_config(instruction_config, &no_env, &default_home(), "/"),
            sessions: std::collections::HashMap::new(),
            seam,
            require: None,
        })
    }

    /// Attaches the copied require vtable (callback endpoints resolve
    /// through it; without one, endpoint providers fail assembly).
    pub const fn attach_require_seam(&mut self, seam: RequireSeam) {
        self.require = Some(seam);
    }

    /// The hosted registry (tests register sections directly).
    #[must_use]
    pub const fn registry(&self) -> &SystemPromptRegistry {
        &self.registry
    }

    /// The mutable hosted registry (tests drive registrations).
    pub const fn registry_mut(&mut self) -> &mut SystemPromptRegistry {
        &mut self.registry
    }
}

fn default_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Serves one method invocation: a pure (JSON-in, JSON-out) function.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`.
pub fn serve(
    service: &mut PromptService,
    method: u32,
    payload: &[u8],
) -> Result<Vec<u8>, ServiceError> {
    match method {
        METHOD_DESCRIBE => Ok(describe_reply()),
        METHOD_ASSEMBLE => {
            let context: WireAssembleContext = decode(payload)?;
            let mut resolver = resolver_of(service);
            let assembly = service
                .registry
                .assemble(&context, &mut resolver)
                .map_err(|error| ServiceError(error.0))?;
            encode(&assembly)
        }
        METHOD_REFRESH_CONTEXT => {
            let request: RefreshRequest = decode(payload)?;
            let mut resolver = resolver_of(service);
            let assembly = service
                .registry
                .refresh_context(&request.assembly, &request.context, &mut resolver)
                .map_err(|error| ServiceError(error.0))?;
            encode(&assembly)
        }
        METHOD_REGISTER_SECTION => register_call(
            payload,
            |registry, spec| {
                let spec: PromptSectionSpec = spec;
                registry.register_section(spec)
            },
            service,
        ),
        METHOD_REGISTER_CONTEXT => register_call(
            payload,
            |registry, spec| {
                let spec: PromptContextSpec = spec;
                registry.register_context(spec)
            },
            service,
        ),
        METHOD_REGISTER_VARIABLE => {
            let request: RegisterVariableRequest = decode(payload)?;
            let result = match request {
                RegisterVariableRequest::Literal { name, value } => service
                    .registry
                    .register_variable(&name, VariableSource::Literal { value }),
                RegisterVariableRequest::Endpoint {
                    name,
                    value_provider,
                } => service
                    .registry
                    .register_variable(&name, VariableSource::Endpoint { value_provider }),
            };
            result.map_err(|error| ServiceError(error.0))?;
            encode(&serde_json::json!({}))
        }
        METHOD_REGISTER_TOOLS => {
            let registration: ToolProviderRegistration = decode(payload)?;
            let token = service.registry.register_tools(registration);
            encode(&serde_json::json!({ "token": token.raw() }))
        }
        METHOD_UNREGISTER => {
            let request: UnregisterRequest = decode(payload)?;
            match request.kind {
                UnregisterKind::Section => {
                    service
                        .registry
                        .unregister_section(&request.name.unwrap_or_default());
                }
                UnregisterKind::Context => {
                    service
                        .registry
                        .unregister_context(&request.name.unwrap_or_default());
                }
                UnregisterKind::Variable => {
                    service
                        .registry
                        .unregister_variable(&request.name.unwrap_or_default());
                }
                UnregisterKind::Tools => {
                    if let Some(token) = request.token {
                        service
                            .registry
                            .unregister_tools(crate::registry::ToolProviderToken::from_raw(token));
                    }
                }
                UnregisterKind::Suppression => {
                    if let Some(token) = request.token {
                        service.registry.release_suppression(
                            crate::registry::SuppressionToken::from_raw(token),
                        );
                    }
                }
            }
            // Absent targets stay no-ops (the dsh disposer is idempotent).
            encode(&serde_json::json!({}))
        }
        METHOD_COMPOSE_INSTRUCTIONS => compose_call(service, payload),
        _ => Err(ServiceError(format!(
            "unknown fr-system-prompt method {method}"
        ))),
    }
}

/// The describe reply document.
fn describe_reply() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "name": "fr-system-prompt",
        "service": SERVICE_NAME,
        "abiVersion": dougong_abi::ABI_VERSION,
    }))
    .unwrap_or_default()
}

/// One registration arm shared by the section/context families.
fn register_call<T: serde::de::DeserializeOwned>(
    payload: &[u8],
    apply: impl FnOnce(&mut SystemPromptRegistry, T) -> Result<(), crate::interpolate::PromptError>,
    service: &mut PromptService,
) -> Result<Vec<u8>, ServiceError> {
    let spec: T = decode(payload)?;
    apply(&mut service.registry, spec).map_err(|error| ServiceError(error.0))?;
    encode(&serde_json::json!({}))
}

/// The compose method arm, split from `serve` for the line budget.
fn compose_call(service: &mut PromptService, payload: &[u8]) -> Result<Vec<u8>, ServiceError> {
    let request: ComposeRequest = decode(payload)?;
    let message = compose(service, &request).map_err(ServiceError)?;
    encode(&ComposeReply { message })
}

fn decode<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, ServiceError> {
    serde_json::from_slice(payload)
        .map_err(|error| ServiceError(format!("malformed payload: {error}")))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ServiceError> {
    serde_json::to_vec(value).map_err(|error| ServiceError(error.to_string()))
}

/// The resolver for one call: the copied vtable, or a failing resolver
/// when no vtable is attached (endpoint providers cannot resolve).
enum ResolverOrStatic {
    Seam(RequireSeam),
    Unattached,
}

impl ProviderResolver for ResolverOrStatic {
    fn text(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<String, String> {
        match self {
            Self::Seam(seam) => seam.text(endpoint, context),
            Self::Unattached => Err(format!(
                "text endpoint {}.{} cannot resolve: no require vtable attached",
                endpoint.service, endpoint.method
            )),
        }
    }

    fn variable(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<Option<String>, String> {
        match self {
            Self::Seam(seam) => seam.variable(endpoint, context),
            Self::Unattached => Err(format!(
                "variable endpoint {}.{} cannot resolve: no require vtable attached",
                endpoint.service, endpoint.method
            )),
        }
    }

    fn tools(
        &mut self,
        endpoint: &EndpointRef,
        context: &WireAssembleContext,
    ) -> Result<ToolProviderResult, String> {
        match self {
            Self::Seam(seam) => seam.tools(endpoint, context),
            Self::Unattached => Err(format!(
                "tools endpoint {}.{} cannot resolve: no require vtable attached",
                endpoint.service, endpoint.method
            )),
        }
    }
}

const fn resolver_of(service: &PromptService) -> ResolverOrStatic {
    match service.require {
        Some(seam) => ResolverOrStatic::Seam(seam),
        None => ResolverOrStatic::Unattached,
    }
}

/// Whether one visible message is an agent-instructions baseline (dsh
/// `visibleBaselineSource` predicate).
/// Fold one reconciled update into the compose outputs and the cache.
fn apply_reconciled_update(
    update: crate::instructions::state::ReconcileOutcome,
    blocks: &mut Vec<InstructionTextBlock>,
    changes: &mut Vec<crate::instructions::render::AgentInstructionChange>,
    session: &mut SessionState,
) {
    let crate::instructions::state::ReconcileOutcome {
        text,
        changes: update_changes,
        version_updates,
    } = update;
    blocks.push(InstructionTextBlock {
        block_type: "text".to_string(),
        text,
    });
    changes.extend(update_changes);
    apply_instruction_version_updates(&mut session.versions, &version_updates);
}

/// The prepared exclusion set for a kept baseline, when one applies (dsh
/// `baselinePreparations` lookup).
fn session_exclusion(
    session: &SessionState,
    identity: &str,
    keep_visible: bool,
) -> Option<HashSet<String>> {
    if !keep_visible {
        return None;
    }
    session
        .preparation
        .as_ref()
        .filter(|preparation| preparation.identity == identity)
        .map(|preparation| preparation.excluded_scopes.clone())
}

/// Assemble the final instruction message document (dsh
/// `createUserMessage` source shape, key order preserved).
fn instruction_message_of(
    blocks: Vec<InstructionTextBlock>,
    changes: &[crate::instructions::render::AgentInstructionChange],
    desired_baseline: bool,
    identity: &str,
) -> InstructionMessage {
    let mut source = serde_json::Map::new();
    source.insert("kind".to_string(), "agent-instructions".into());
    source.insert("form".to_string(), "instructions".into());
    if desired_baseline {
        source.insert("baseline".to_string(), true.into());
        source.insert("baselineIdentity".to_string(), identity.into());
    }
    source.insert(
        "changes".to_string(),
        serde_json::to_value(changes).unwrap_or_default(),
    );
    InstructionMessage {
        content: blocks,
        source,
    }
}

/// The prepared-identity short circuit, when it applies (dsh compose
/// head).
fn prepared_head(
    service: &PromptService,
    request: &ComposeRequest,
    identity: &str,
) -> Option<InstructionMessage> {
    if !request.touched_paths.is_empty() || request.pending.is_empty() {
        return None;
    }
    let prepared = service
        .sessions
        .get(&request.session_id)
        .and_then(|state| state.preparation.as_ref())?;
    if prepared.identity != identity {
        return None;
    }
    let head = &request.pending[0];
    Some(InstructionMessage {
        content: vec![InstructionTextBlock {
            block_type: "text".to_string(),
            text: text_of(&head.content),
        }],
        source: head.source.clone(),
    })
}

/// The claimed authority list and the visible baseline source (dsh
/// `visibleBaselineSource`: newest authority baseline first, then newest
/// surface baseline).
struct BaselineVisibility {
    authority: Vec<VisibleMessage>,
    source: Option<serde_json::Map<String, serde_json::Value>>,
}

fn baseline_visibility(request: &ComposeRequest) -> BaselineVisibility {
    let authority: Vec<VisibleMessage> = request.claimed.clone();
    let source = authority
        .iter()
        .rev()
        .find(|message| is_baseline(message))
        .map(|message| message.source.clone())
        .or_else(|| {
            request
                .surface
                .iter()
                .rev()
                .find(|message| is_baseline(message))
                .map(|message| message.source.clone())
        });
    BaselineVisibility { authority, source }
}

fn is_baseline(message: &VisibleMessage) -> bool {
    message
        .source
        .get("kind")
        .and_then(serde_json::Value::as_str)
        == Some("agent-instructions")
        && message
            .source
            .get("baseline")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

/// Compose the workspace-instruction message for one boundary (dsh
/// agent-instructions/src/index.ts `compose`, verbatim flow).
///
/// # Errors
///
/// Returns the first discovery, read, or reconciliation failure.
fn compose(
    service: &mut PromptService,
    request: &ComposeRequest,
) -> Result<Option<InstructionMessage>, String> {
    let config = &service.compose_config;
    if config.max_bytes <= 0 {
        return Ok(None);
    }
    let project_root = find_project_root(
        &request.cwd,
        &config.discovery.project_root_markers,
        service.seam.as_mut(),
    )?;
    let identity = crate::instructions::config::workspace_baseline_identity(
        config,
        &request.cwd,
        &project_root,
    );
    // Same-boundary short circuit: a prepared identity and an unchanged
    // pending projection return the pending head verbatim.
    if let Some(short) = prepared_head(service, request, &identity) {
        return Ok(Some(short));
    }
    let visibility = baseline_visibility(request);
    let authority = visibility.authority;
    let visible_baseline = visibility.source;
    let baseline_present = visible_baseline.is_some();
    let keep_visible = visible_baseline
        .as_ref()
        .and_then(|source| source.get("baselineIdentity"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|visible| visible == identity);
    let session = service
        .sessions
        .entry(request.session_id.clone())
        .or_default();
    let excluded_baseline_scopes = session_exclusion(session, &identity, keep_visible);
    let stage = baseline_stage(
        BaselineStage {
            discovery: &config.discovery,
            seam: service.seam.as_mut(),
            session,
            cwd: &request.cwd,
            project_root: &project_root,
            identity: &identity,
            max_bytes: compose_budget(config),
            max_source_bytes: config.max_source_bytes,
        },
        &BaselineInputs {
            baseline_present,
            keep_visible,
            visible_baseline: visible_baseline.as_ref(),
            excluded_prepared: excluded_baseline_scopes.is_some(),
        },
    );
    let mut content_blocks = stage.blocks;
    let mut changes = stage.changes;
    let authority = match stage.authority_push {
        Some(push) => {
            let mut extended = authority;
            extended.push(push);
            extended
        }
        None => authority,
    };
    if let Some(preparation) = stage.preparation {
        session.preparation = Some(preparation);
    }
    let update = reconcile_instruction_context(
        &ReconcileInput {
            discovery: &config.discovery,
            max_bytes: compose_budget(config),
            max_source_bytes: config.max_source_bytes,
            cwd: &request.cwd,
            project_root: &project_root,
            surface: &request.surface,
            authority: &authority,
            scope_messages: &request.pending,
            touched_paths: &request.touched_paths,
            include_baseline_scopes: keep_visible,
            excluded_baseline_scopes: excluded_baseline_scopes.as_ref(),
        },
        &mut session.versions,
        service.seam.as_mut(),
    );
    if let Some(update) = update {
        apply_reconciled_update(update, &mut content_blocks, &mut changes, session);
    }
    if content_blocks.is_empty() {
        return Ok(None);
    }
    Ok(Some(instruction_message_of(
        content_blocks,
        &changes,
        stage.desired_baseline,
        &identity,
    )))
}

/// The compose byte budget as `usize` (callers gate on `max_bytes <= 0`
/// first, so the conversion is total here).
fn compose_budget(config: &ResolvedInstructionConfig) -> usize {
    usize::try_from(config.max_bytes).expect("compose gates non-positive budgets first")
}

/// The borrow aggregation for one baseline stage run.
struct BaselineStage<'a> {
    discovery: &'a crate::instructions::config::ResolvedDiscovery,
    seam: &'a mut dyn FileSeam,
    session: &'a mut SessionState,
    cwd: &'a str,
    project_root: &'a str,
    identity: &'a str,
    max_bytes: usize,
    max_source_bytes: u64,
}

/// The baseline visibility inputs.
struct BaselineInputs<'a> {
    baseline_present: bool,
    keep_visible: bool,
    visible_baseline: Option<&'a serde_json::Map<String, serde_json::Value>>,
    excluded_prepared: bool,
}

/// One baseline stage's outputs.
struct BaselineOutcome {
    preparation: Option<Preparation>,
    blocks: Vec<InstructionTextBlock>,
    changes: Vec<crate::instructions::render::AgentInstructionChange>,
    authority_push: Option<VisibleMessage>,
    desired_baseline: bool,
}

/// Load, dedup-account, and render the startup baseline (dsh `compose`
/// baseline block, verbatim flow): the excluded-scope computation keeps
/// hidden duplicates promotable, version states seed the session cache,
/// and a fresh baseline pushes its own authority message.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the stage carries `&mut` seam and session borrows that the body moves out"
)]
fn baseline_stage(stage: BaselineStage<'_>, inputs: &BaselineInputs<'_>) -> BaselineOutcome {
    let mut outcome = BaselineOutcome {
        preparation: None,
        blocks: Vec::new(),
        changes: Vec::new(),
        authority_push: None,
        desired_baseline: false,
    };
    if inputs.baseline_present && inputs.keep_visible && inputs.excluded_prepared {
        return outcome;
    }
    let replace_previous = inputs.baseline_present && !inputs.keep_visible;
    let Some(instructions) = load_baseline_instruction_set(
        stage.discovery,
        stage.cwd,
        stage.project_root,
        stage.max_bytes,
        stage.max_source_bytes,
        replace_previous,
        stage.seam,
    ) else {
        return outcome;
    };
    let (baseline, versions) = baseline_instruction_state(&instructions.included);
    let mut included_digests_by_group = std::collections::HashMap::new();
    for file in &instructions.included {
        let scope = crate::instructions::render::scope_for_display_path(&file.display_path);
        let group = crate::instructions::render::instruction_candidate_group(&scope);
        let digest = crate::instructions::digest::trimmed_instruction_digest(&file.content);
        included_digests_by_group
            .entry(group)
            .or_insert_with(HashSet::new)
            .insert(digest);
    }
    // A duplicate inherits its winner's budget omission; only duplicates
    // of represented content stay promotable.
    let mut excluded_scopes: HashSet<String> =
        observed_changes_scopes(&instructions).into_iter().collect();
    for scope in baseline.keys() {
        excluded_scopes.remove(scope);
    }
    for file in &instructions.deduped {
        let display = &file.display_path;
        let scope = crate::instructions::render::scope_for_display_path(display);
        let group = crate::instructions::render::instruction_candidate_group(&scope);
        let digest = crate::instructions::digest::trimmed_instruction_digest(&file.content);
        if included_digests_by_group
            .get(&group)
            .is_some_and(|digests| digests.contains(&digest))
        {
            excluded_scopes.remove(&crate::instructions::render::instruction_scope_key(display));
        }
    }
    outcome.preparation = Some(Preparation {
        identity: stage.identity.to_string(),
        excluded_scopes,
    });
    for (scope, state) in versions {
        stage.session.versions.insert(scope, state);
    }
    if !inputs.keep_visible && !instructions.rendered.text.is_empty() {
        outcome.blocks.push(InstructionTextBlock {
            block_type: "text".to_string(),
            text: instructions.rendered.text.clone(),
        });
        let mut baseline_changes: Vec<_> = baseline.values().cloned().collect();
        if replace_previous && let Some(previous) = inputs.visible_baseline {
            let replacement_scopes: HashSet<String> =
                baseline_changes.iter().map(|c| c.scope.clone()).collect();
            for change in change_list(previous) {
                let replaced = change.action == InstructionChangeAction::Remove
                    || replacement_scopes.contains(&change.scope);
                if !replaced {
                    baseline_changes.push(crate::instructions::render::AgentInstructionChange {
                        action: InstructionChangeAction::Remove,
                        scope: change.scope.clone(),
                        path: change.path.clone(),
                        digest: None,
                    });
                }
            }
        }
        outcome.changes.extend(baseline_changes);
        outcome.authority_push = Some(baseline_message(
            &instructions.rendered.text,
            stage.identity,
            &outcome.changes,
        ));
        outcome.desired_baseline = true;
    }
    outcome
}

/// The observed set's change scopes (deduplicated view of what the
/// baseline load saw, before exclusion computation).
fn observed_changes_scopes(
    instructions: &crate::instructions::files::RenderedInstructionSet,
) -> Vec<String> {
    let (observed_changes, _) = baseline_instruction_state(&instructions.observed);
    observed_changes.into_keys().collect()
}

/// Parse one source document's change list (tolerant).
fn change_list(
    source: &serde_json::Map<String, serde_json::Value>,
) -> Vec<crate::instructions::render::AgentInstructionChange> {
    crate::instructions::state::workspace_instruction_changes(source)
}

/// The baseline authority message pushed during compose (dsh
/// `createUserMessage` shape).
fn baseline_message(
    text: &str,
    identity: &str,
    changes: &[crate::instructions::render::AgentInstructionChange],
) -> VisibleMessage {
    let mut source = serde_json::Map::new();
    source.insert("kind".to_string(), "agent-instructions".into());
    source.insert("form".to_string(), "instructions".into());
    source.insert("baseline".to_string(), true.into());
    source.insert("baselineIdentity".to_string(), identity.into());
    source.insert(
        "changes".to_string(),
        serde_json::to_value(changes).unwrap_or_default(),
    );
    VisibleMessage {
        source,
        content: serde_json::json!([{ "type": "text", "text": text }]),
    }
}

/// The first text block's text, or the raw JSON (same-payload comparisons
/// only need a deterministic projection).
fn text_of(content: &serde_json::Value) -> String {
    content
        .as_array()
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(|text| text.as_str())
        .unwrap_or_default()
        .to_string()
}

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::files::MemoryFs;

    fn service(seam: MemoryFs) -> PromptService {
        PromptService::new(
            &RegistryConfig::default(),
            &InstructionConfig::base_patch(),
            Box::new(seam),
        )
        .expect("service")
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let mut service = service(MemoryFs::new());
        let reply = serve(&mut service, METHOD_DESCRIBE, b"{}").expect("describe");
        let parsed: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(parsed["service"], SERVICE_NAME);
        assert_eq!(parsed["abiVersion"], dougong_abi::ABI_VERSION);
    }

    #[test]
    fn assemble_serves_the_default_sections() {
        let mut service = service(MemoryFs::new());
        let reply = serve(&mut service, METHOD_ASSEMBLE, b"{}").expect("assemble");
        let assembly: PromptAssembly = serde_json::from_slice(&reply).expect("assembly");
        assert!(
            assembly
                .sections
                .iter()
                .any(|section| section.name == "harness:identity")
        );
    }

    #[test]
    fn register_section_round_trips_and_duplicates_fail() {
        let mut service = service(MemoryFs::new());
        let spec = serde_json::json!({
            "name": "guide",
            "order": 100.0,
            "text": "guidance"
        });
        serve(
            &mut service,
            METHOD_REGISTER_SECTION,
            spec.to_string().as_bytes(),
        )
        .expect("register");
        let failure = serve(
            &mut service,
            METHOD_REGISTER_SECTION,
            spec.to_string().as_bytes(),
        )
        .expect_err("duplicate");
        assert!(
            failure
                .0
                .contains("prompt section \"guide\" is already registered"),
            "{}",
            failure.0
        );
        let reply = serve(&mut service, METHOD_ASSEMBLE, b"{}").expect("assemble");
        let assembly: PromptAssembly = serde_json::from_slice(&reply).expect("assembly");
        assert!(assembly.sections.iter().any(|s| s.name == "guide"));
    }

    #[test]
    fn register_variable_accepts_both_shapes() {
        let mut service = service(MemoryFs::new());
        serve(
            &mut service,
            METHOD_REGISTER_VARIABLE,
            br#"{"name":"cwd","value":"/w"}"#,
        )
        .expect("static variable");
        serve(
            &mut service,
            METHOD_REGISTER_VARIABLE,
            br#"{"name":"home","valueProvider":{"service":"env","method":2}}"#,
        )
        .expect("endpoint variable");
        // The endpoint variable cannot resolve without a vtable; assembly
        // referencing it fails, proving both shapes registered.
        let section = br#"{"name":"uses","order":10,"text":"{{home}}"}"#;
        serve(&mut service, METHOD_REGISTER_SECTION, section).expect("section");
        let failure = serve(&mut service, METHOD_ASSEMBLE, b"{}").expect_err("endpoint fails");
        assert!(
            failure.0.contains("no require vtable attached"),
            "{}",
            failure.0
        );
    }

    #[test]
    fn register_tools_returns_a_token_and_unregister_is_idempotent() {
        let mut service = service(MemoryFs::new());
        let reply = serve(
            &mut service,
            METHOD_REGISTER_TOOLS,
            br#"{"provider":{"service":"tools","method":6}}"#,
        )
        .expect("register tools");
        let parsed: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        let token = parsed["token"].as_u64().expect("token");
        let unregister = serde_json::json!({ "kind": "tools", "token": token });
        serve(
            &mut service,
            METHOD_UNREGISTER,
            unregister.to_string().as_bytes(),
        )
        .expect("unregister");
        serve(
            &mut service,
            METHOD_UNREGISTER,
            unregister.to_string().as_bytes(),
        )
        .expect("unregister again is a no-op");
    }

    #[test]
    fn compose_emits_the_baseline_frame() {
        let mut seam = MemoryFs::new();
        seam.add_directory("/w/proj");
        seam.write("/w/proj/.git", "");
        seam.write("/w/proj/AGENTS.md", "project guidance");
        let mut service = service(seam);
        let request = ComposeRequest::new("s1", "/w/proj");
        let reply = serve(
            &mut service,
            METHOD_COMPOSE_INSTRUCTIONS,
            &serde_json::to_vec(&request).expect("json"),
        )
        .expect("compose");
        let parsed: ComposeReply = serde_json::from_slice(&reply).expect("reply");
        let message = parsed.message.expect("baseline message");
        assert_eq!(
            message.source.get("baseline"),
            Some(&serde_json::json!(true))
        );
        assert!(message.content[0].text.contains("project guidance"));
        assert!(
            message.content[0]
                .text
                .contains("Instructions from: AGENTS.md"),
            "the first line names the path: {:?}",
            message.content[0].text
        );
        assert!(message.content[0].text.contains("<system-reminder>"));
        assert!(
            message
                .source
                .get("changes")
                .and_then(|c| c.as_array())
                .is_some_and(|c| !c.is_empty())
        );
    }

    #[test]
    fn compose_without_instructions_returns_none() {
        let mut seam = MemoryFs::new();
        seam.add_directory("/w/proj");
        let mut service = service(seam);
        let request = ComposeRequest::new("s1", "/w/proj");
        let reply = serve(
            &mut service,
            METHOD_COMPOSE_INSTRUCTIONS,
            &serde_json::to_vec(&request).expect("json"),
        )
        .expect("compose");
        let parsed: ComposeReply = serde_json::from_slice(&reply).expect("reply");
        assert!(parsed.message.is_none());
    }

    #[test]
    fn compose_short_circuits_on_a_prepared_identity() {
        let mut seam = MemoryFs::new();
        seam.add_directory("/w/proj");
        seam.write("/w/proj/.git", "");
        seam.write("/w/proj/AGENTS.md", "guide");
        let mut service = service(seam);
        let first = serve(
            &mut service,
            METHOD_COMPOSE_INSTRUCTIONS,
            &serde_json::to_vec(&ComposeRequest::new("s1", "/w/proj")).expect("json"),
        )
        .expect("first compose");
        let first: ComposeReply = serde_json::from_slice(&first).expect("reply");
        let baseline = first.message.expect("baseline");
        // The next boundary with the pending projection unchanged returns
        // the pending head verbatim.
        let mut request = ComposeRequest::new("s1", "/w/proj");
        request.pending = vec![VisibleMessage {
            source: baseline.source.clone(),
            content: serde_json::json!([{ "type": "text", "text": "pending text" }]),
        }];
        let reply = serve(
            &mut service,
            METHOD_COMPOSE_INSTRUCTIONS,
            &serde_json::to_vec(&request).expect("json"),
        )
        .expect("short-circuit compose");
        let parsed: ComposeReply = serde_json::from_slice(&reply).expect("reply");
        let message = parsed.message.expect("pending head");
        assert_eq!(message.source, baseline.source);
        assert_eq!(message.content[0].text, "pending text");
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        let mut service = service(MemoryFs::new());
        let failure = serve(&mut service, 99, b"{}").expect_err("unknown");
        assert!(
            failure.0.contains("unknown fr-system-prompt method 99"),
            "{}",
            failure.0
        );
    }
}
