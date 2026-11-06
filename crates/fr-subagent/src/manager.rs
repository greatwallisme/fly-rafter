// Rust guideline compliant 2026-11-06
//! The consumed seams' contract (the dsh opportunistic `ctx.get` faces
//! made explicit for the fr wire): every cross-plugin dependency the
//! continuation manager touches, behind one trait so the domain tests
//! script fakes (the fr-tools-builtin `Seams` judgment).
//!
//! All shapes travel as `serde_json::Value`; the wire DTOs stay
//! single-sourced at the provider crates and this trait speaks the
//! already-encoded documents.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::domain::SubagentError;

/// One cold observation of a persisted or live session (the dsh
/// `SessionObservation` cut fr-subagent reads).
#[derive(Debug, Clone)]
pub struct Observation {
    /// Whether the cut came from the live store or a stored log.
    pub source: String,
    /// The immutable session header.
    pub header: Value,
    /// The fork-inherited event count paired with the header.
    pub inherited_event_count: u64,
    /// The contiguous events at the cut.
    pub events: Vec<Value>,
}

/// The child-creation request over the agent seam (the dsh
/// `agents.create` options this manager fills).
#[derive(Debug, Clone)]
pub struct ChildCreate {
    /// The reserved durable child session id.
    pub session_id: String,
    /// The absolute initial directory.
    pub cwd: String,
    /// The optional completed-turn prefix a fork seeds.
    pub seed: Option<Vec<Value>>,
    /// The fork-inherited prefix length (zero for a fresh child).
    pub inherited_event_count: u64,
    /// The durable creation metadata.
    pub meta: Value,
    /// The resolved route fields.
    pub route: ChildRoute,
}

/// The four route fields the child inherits or overrides (dsh
/// `AgentOptions`).
#[derive(Debug, Clone, Default)]
pub struct ChildRoute {
    /// The provider id.
    pub provider: Option<String>,
    /// The model id.
    pub model: Option<String>,
    /// The reasoning effort.
    pub reasoning_effort: Option<Value>,
    /// The output-token ceiling.
    pub max_tokens: Option<u64>,
}

/// The cross-plugin dependency surface of the continuation manager.
pub trait Seams: Send + Sync {
    /// The live-session read over the persistence face (the dsh
    /// `agent.session` snapshot read).
    fn session_read(&self, id: &str) -> Result<Observation, String>;

    /// The header's latest request-header snapshot (dsh
    /// `session.requestHeader()`), or null when none was logged.
    fn request_header(&self, id: &str) -> Result<Option<Value>, String>;

    /// One event appended to a live session's log (dsh
    /// `session.append`).
    fn session_append(&self, id: &str, event_type: &str, data: &Value) -> Result<(), String>;

    /// The liveness probe (dsh `agents.get(id)` returning undefined).
    fn agent_live(&self, id: &str) -> Result<bool, String>;

    /// The loop status snapshot (dsh `agent.status`).
    fn agent_status(&self, id: &str) -> Result<&'static str, String>;

    /// Creates the child session and attaches its driver (dsh
    /// `agents.create`).
    fn agent_create(&self, create: &ChildCreate) -> Result<(), String>;

    /// Adopts one persisted session and attaches its driver (dsh
    /// `agents.resume`).
    fn agent_resume(
        &self,
        session_id: &str,
        route: &ChildRoute,
        meta_patch: Option<Value>,
    ) -> Result<(), String>;

    /// Queues one message as the child's next turn (dsh
    /// `agent.followup`).
    fn agent_followup(&self, id: &str, message: &Value) -> Result<(), String>;

    /// Admits one message at the nearest step (dsh `agent.steer`).
    fn agent_steer(&self, id: &str, message: &Value) -> Result<(), String>;

    /// Injects without waking (dsh `agent.inject`).
    fn agent_inject(&self, id: &str, message: &Value) -> Result<(), String>;

    /// Cancels the target's current work (dsh `agent.cancel`).
    fn agent_cancel(&self, id: &str, cause: &Value, keep_inbox: bool) -> Result<(), String>;

    /// Flushes the session's persistence tail (dsh `sessions.flush`).
    fn session_flush(&self, id: &str) -> Result<(), String>;

    /// Whether a stored log exists for the id (dsh `persistence.stat`).
    fn persisted_exists(&self, id: &str) -> Result<bool, String>;

    /// Observes one live or stored session (dsh `observeSession`).
    fn observe(&self, id: &str) -> Result<Observation, String>;

    /// Reads one projection's state (dsh `sessionProjections` state-of).
    fn projection_state(&self, id: &str, key: &str) -> Result<Option<Value>, String>;

    /// The parent's effective directory (dsh
    /// `workingDirectory.ensure`).
    fn working_directory_ensure(&self, agent: &str) -> Result<String, String>;

    /// Pins the child's directory (dsh `workingDirectory.set`).
    fn working_directory_set(&self, agent: &str, path: &str) -> Result<(), String>;

    /// Whether the approval capability is composed (dsh
    /// `ctx.get('approval') !== undefined`).
    fn approval_available(&self) -> Result<bool, String>;

    /// Publishes one lifecycle edge (the dsh `subagent/start`/`end`
    /// scoped emit).
    fn emit_lifecycle(&self, event: &str, payload: &Value);
}

// ---------------------------------------------------------------------------
// The continuation manager
// ---------------------------------------------------------------------------

/// One advertised start-time capability (dsh `SubagentCapabilities`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Supports provider/model/effort/max-tokens overrides.
    pub agent_options: bool,
    /// Supports an independent agent preset selection.
    pub agent_preset: bool,
    /// Supports the scoped structured-output runtime.
    pub output_schema: bool,
    /// Enforces a numeric recursion cap.
    pub depth_limit: bool,
}

/// The local provider's detached creation contribution (dsh
/// `prepareContinuable`): the optional completed-turn seed.
pub type SeedFn =
    Arc<dyn Fn(&dyn Seams, &str, &str) -> Result<Option<Vec<Value>>, SubagentError> + Send + Sync>;

/// The external provider's owned execution (dsh `SubagentProvider.start`).
pub type StartFn = Arc<dyn Fn(Value) -> Result<ExternalRun, SubagentError> + Send + Sync>;

/// The external execution handle (dsh `SubagentRun`).
#[derive(Clone)]
pub struct ExternalRun {
    /// The flattened, never-rejecting terminal result.
    pub result: crate::domain::SubagentResult,
    /// Applies the external stop (dsh `run.dispose` + controller abort).
    pub stop: Arc<dyn Fn(&Value) + Send + Sync>,
}

/// One registered delegation backend.
#[derive(Clone)]
pub struct Provider {
    /// The unique registry name (`spawn`, `fork`, `acp`).
    pub name: String,
    /// The advertised start-time capabilities.
    pub capabilities: Capabilities,
    /// Whether the child sees the parent's completed-turn prefix.
    pub inherits_parent_context: bool,
    /// The execution kind and its contribution.
    pub kind: ProviderKind,
}

/// The provider's execution kind.
#[derive(Clone)]
pub enum ProviderKind {
    /// Continuable local children: the provider contributes only the
    /// detached creation inputs (the seed).
    Local {
        /// The seed contribution (None for a fresh child).
        seed: SeedFn,
    },
    /// One external execution without a local child session.
    External {
        /// The backend launcher.
        start: StartFn,
    },
}

/// The host configuration (dsh `SubagentRuntime.Config`).
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// The default delegation depth for tools without an explicit limit.
    pub max_depth: u64,
    /// The maximum live children sharing one root's pool.
    pub max_active: u64,
}

/// One materialization's shared capacity (dsh `ActivationPool`).
#[derive(Debug, Default)]
pub struct ActivationPool {
    /// The live slot count.
    slots: std::sync::atomic::AtomicU64,
}

impl ActivationPool {
    /// Reserves one slot before reconstruction (dsh `ActivationPool.reserve`).
    ///
    /// # Errors
    ///
    /// The verbatim activation-limit refusal at capacity.
    fn reserve(&self, capacity: u64) -> Result<(), SubagentError> {
        let live = self.slots.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
        if live > capacity {
            self.slots.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            return Err(SubagentError::activation_limit(capacity));
        }
        Ok(())
    }

    /// Returns one slot after disposal.
    fn release(&self) {
        self.slots.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// The execution resources of one resident activation.
#[derive(Clone)]
enum Resource {
    /// The local child driver and its log boundary.
    Local {
        /// The event count at materialization (the result suffix start).
        output_start: u64,
        /// The structured runtime, when one was requested.
        structured: Option<Arc<Mutex<crate::structured::StructuredRuntime>>>,
        /// Whether the failure marker is live.
        failed: bool,
    },
    /// The external execution handle.
    External {
        /// The owned run.
        run: ExternalRun,
        /// Whether the stop signal was applied.
        stopped: bool,
    },
}

/// One admitted residency epoch (dsh `Activation`).
#[derive(Clone)]
struct Activation {
    /// The durable child session id.
    child_id: String,
    /// The establishing provider name.
    provider: String,
    /// The durable direct parent id.
    parent_id: String,
    /// The exact ancestry observed at admission (nearest first).
    ancestry: Vec<String>,
    /// The shared capacity pool.
    pool: Arc<ActivationPool>,
    /// The execution resources.
    resource: Resource,
    /// Whether any delivery was ever accepted (announced).
    announced: bool,
    /// Whether the settlement path has run.
    settled: bool,
    /// The session ids this activation owns (block its settlement).
    owned_children: std::collections::BTreeSet<String>,
}

/// The delegation-depth resolution input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthPolicy {
    /// No numeric cap (the provider owns enforcement).
    ProviderManaged,
    /// The explicit non-negative cap.
    Numeric(u64),
}

/// The resolved child route (dsh `resolveChildAgentOptions`).
#[must_use]
pub fn resolve_child_route(
    parent_route: &ChildRoute,
    requested: &ChildRoute,
    route_changed_clears_effort: bool,
) -> ChildRoute {
    let mut resolved = ChildRoute {
        provider: requested
            .provider
            .clone()
            .or_else(|| parent_route.provider.clone()),
        model: requested
            .model
            .clone()
            .or_else(|| parent_route.model.clone()),
        reasoning_effort: requested
            .reasoning_effort
            .clone()
            .or_else(|| parent_route.reasoning_effort.clone()),
        max_tokens: requested.max_tokens.or(parent_route.max_tokens),
    };
    let route_changed =
        resolved.provider != parent_route.provider || resolved.model != parent_route.model;
    if route_changed && requested.reasoning_effort.is_none() && route_changed_clears_effort {
        resolved.reasoning_effort = None;
    }
    resolved
}

/// One live continuation manager (dsh `SubagentManager` over the fr
/// seams: same admission order, same settlement state machine, the
/// cancel/authority matrix, and the cold-resume reconstruction — the
/// fr carrier replaces cordis effects and scoped contexts with the
/// wire registrations and the per-child serialized lock).
pub struct Manager {
    seams: Arc<dyn Seams>,
    config: Config,
    /// The registered providers by name (insertion order kept for
    /// deterministic listing).
    providers: Mutex<Vec<Provider>>,
    /// The resident activations by durable child id.
    resident: Mutex<std::collections::BTreeMap<String, Activation>>,
    /// The per-child operation serializer (dsh `ChildLock`).
    locks: Mutex<std::collections::BTreeMap<String, Arc<Mutex<()>>>>,
    /// The managed session ids by their root, for the join probe.
    roots: Mutex<std::collections::BTreeMap<String, Arc<ActivationPool>>>,
}

impl Manager {
    /// One manager over the consumed seams and the host configuration.
    #[must_use]
    pub fn new(seams: Arc<dyn Seams>, config: Config) -> Self {
        Self {
            seams,
            config,
            providers: Mutex::new(Vec::new()),
            resident: Mutex::new(std::collections::BTreeMap::new()),
            locks: Mutex::new(std::collections::BTreeMap::new()),
            roots: Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    /// Registers one provider (dsh `registerProvider`): the name is
    /// unique, and a provider without either execution method refuses
    /// before registration.
    ///
    /// # Errors
    ///
    /// The duplicate-name or missing-method refusal.
    pub fn register_provider(&self, provider: Provider) -> Result<(), SubagentError> {
        let mut providers = self.providers.lock().expect("providers lock");
        if providers
            .iter()
            .any(|existing| existing.name == provider.name)
        {
            return Err(SubagentError::new(
                "DUPLICATE_PROVIDER",
                format!(
                    "a subagent provider named \"{}\" is already registered",
                    provider.name
                ),
            ));
        }
        providers.push(provider);
        Ok(())
    }

    /// Lists the registered provider names in insertion order (dsh
    /// `list`).
    #[must_use]
    pub fn provider_names(&self) -> Vec<String> {
        self.providers
            .lock()
            .expect("providers lock")
            .iter()
            .map(|provider| provider.name.clone())
            .collect()
    }

    /// Resolves one provider by name (dsh `expectProvider`).
    ///
    /// # Errors
    ///
    /// The verbatim no-provider refusal.
    fn provider(&self, name: &str) -> Result<Provider, SubagentError> {
        self.providers
            .lock()
            .expect("providers lock")
            .iter()
            .find(|provider| provider.name == name)
            .cloned()
            .ok_or_else(|| {
                SubagentError::new(
                    "NO_PROVIDER",
                    format!("no subagent provider registered for \"{name}\""),
                )
            })
    }

    /// The per-child operation mutex (dsh `ChildLock.run`).
    fn child_lock(&self, child_id: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .expect("locks lock")
            .entry(child_id.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// The host configuration snapshot (the runtime face's resolver).
    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// The persisted delegation depth of one session (the depth-context
    /// provider's read).
    #[must_use]
    pub fn header_depth(&self, session_id: &str) -> Result<u64, String> {
        let observation = self.seams.observe(session_id)?;
        crate::depth::delegation_depth_of(
            observation
                .header
                .get("delegationDepth")
                .and_then(serde_json::Value::as_u64),
            None,
        )
    }

    /// Whether one session is a delegated child of any live activation
    /// (the delegation-context provider's filter).
    #[must_use]
    pub fn is_delegated_child(&self, session_id: &str) -> bool {
        let resident = self.resident.lock().expect("resident lock");
        resident
            .values()
            .any(|activation| activation.child_id == session_id)
    }

    /// The live structured runtime of one session (the scoped provider
    /// and section endpoints' read).
    #[must_use]
    pub fn structured_runtime(
        &self,
        session_id: &str,
    ) -> Option<Arc<Mutex<crate::structured::StructuredRuntime>>> {
        let resident = self.resident.lock().expect("resident lock");
        for activation in resident.values() {
            if activation.child_id == session_id {
                if let Resource::Local { structured, .. } = &activation.resource {
                    return structured.clone();
                }
            }
        }
        None
    }

    /// Whether one session's loop is currently running (the listing
    /// status projection).
    #[must_use]
    pub fn is_running(&self, session_id: &str) -> bool {
        self.seams.agent_status(session_id).unwrap_or("idle") == "running"
    }

    /// Resolves the delegation tool's depth policy against the current
    /// host setting (dsh `resolveMaxDepth`).
    #[must_use]
    pub fn resolve_max_depth(&self, configured: DepthPolicy) -> Option<u64> {
        match configured {
            DepthPolicy::ProviderManaged => None,
            DepthPolicy::Numeric(value) => Some(value),
        }
    }

    /// Starts one continuable local child and resolves at initial inbox
    /// acceptance (dsh `startLocal`): every earlier failure leaves no
    /// activation, no catalog row, and no owned id.
    ///
    /// # Errors
    ///
    /// The coded refusal (duplicate, capacity, depth, capability,
    /// persistence, or transport).
    pub fn start_local(&self, request: &StartRequest) -> Result<StartReceipt, SubagentError> {
        let provider = self.provider(&request.provider)?;
        let ProviderKind::Local { seed } = &provider.kind else {
            return Err(SubagentError::new(
                "UNSUPPORTED_CAPABILITY",
                "reserved child ids require a local backend".to_owned(),
            ));
        };
        self.assert_capabilities(&provider, request)?;
        if !self.seams.agent_live(&request.parent_id).unwrap_or(false) {
            return Err(SubagentError::unauthorized(
                "subagent creation requires the exact live parent agent",
            ));
        }
        if let Some(schema) = &request.output_schema {
            crate::schema::assert_object_schema(schema).map_err(|detail| {
                SubagentError::new(
                    "UNSUPPORTED_CAPABILITY",
                    format!("invalid output schema: {detail}"),
                )
            })?;
        }
        let child_id = request
            .child_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.assert_child_id_available(&child_id)?;
        let parent_depth = self.parent_depth(&request.parent_id)?;
        let child_depth = crate::depth::resolve_child_depth(parent_depth, request.max_depth)
            .map_err(|error| SubagentError::new(error.code, error.message))?;
        let route = resolve_child_route(
            &self.parent_route(&request.parent_id)?,
            &request.route,
            provider.agent_route_defaults().is_none(),
        );
        let descriptor = crate::descriptor::snapshot(&crate::descriptor::ContinuableDescriptor {
            provider: provider.name.clone(),
            label: request.label.clone(),
            agent_provider: route.provider.clone(),
            agent_model: route.model.clone(),
            agent_reasoning_effort: route
                .reasoning_effort
                .as_ref()
                .and_then(|effort| effort.as_str())
                .map(str::to_owned),
            persona: None,
            tool_filter: None,
        });
        let delegated = self.capture_delegated_policies(&request.parent_id);
        let cwd = self.resolve_directory(&request.parent_id, request.cwd.as_deref())?;
        let seeded = seed(self.seams.as_ref(), &request.parent_id, &cwd)?;
        let inherited_event_count = seeded.as_ref().map_or(0, Vec::len);
        let structured = request.output_schema.as_ref().map(|schema| {
            Arc::new(Mutex::new(crate::structured::StructuredRuntime::new(
                schema.clone(),
            )))
        });

        let child_mutex = self.child_lock(&child_id);
        let _guard = child_mutex.lock().expect("child lock");
        // Re-check under the per-child serialization (dsh re-checks
        // admission and availability inside `locks.run`).
        self.assert_child_id_available(&child_id)?;
        if request.child_id.is_some() && self.seams.persisted_exists(&child_id).unwrap_or(false) {
            return Err(SubagentError::duplicate_child(&child_id));
        }
        let pool = self.pool_for(&request.parent_id);
        pool.reserve(self.config.max_active)?;
        let activation = Activation {
            child_id: child_id.clone(),
            provider: provider.name.clone(),
            parent_id: request.parent_id.clone(),
            ancestry: self.lineage_of(&request.parent_id),
            pool,
            resource: Resource::Local {
                output_start: inherited_event_count as u64,
                structured,
                failed: false,
            },
            announced: false,
            settled: false,
            owned_children: std::collections::BTreeSet::new(),
        };
        self.materialize_local(
            &activation,
            &ChildCreate {
                session_id: child_id.clone(),
                cwd,
                seed: seeded,
                inherited_event_count: u64::try_from(inherited_event_count).unwrap_or(u64::MAX),
                meta: child_meta(
                    &request.parent_id,
                    child_depth,
                    inherited_event_count > 0,
                    request.agent_preset.as_deref(),
                ),
                route,
            },
            &descriptor,
            &delegated,
            request.agent_preset.as_deref(),
        )?;
        self.resident
            .lock()
            .expect("resident lock")
            .insert(child_id.clone(), activation);
        let message_id = self.submit_initial(
            &child_id,
            &request.parent_id,
            &request.prompt,
            provider.inherits_parent_context,
        )?;
        // The catalog row commits after the initial acceptance (dsh
        // `establishCatalogChild` inside the materialized commit).
        self.establish_catalog(&request.parent_id, &child_id, &request.label)?;
        Ok(StartReceipt {
            child_id,
            message_id: Some(message_id),
        })
    }

    /// Starts one external backend execution under activation ownership
    /// (dsh `SubagentManager.startExternal`): the provider's run handle
    /// transfers to the activation; every earlier failure disposes the
    /// partial execution and rolls back.
    ///
    /// # Errors
    ///
    /// The provider, capacity, or parent-liveness refusal.
    pub fn start_external(&self, request: &StartRequest) -> Result<StartReceipt, SubagentError> {
        let provider = self.provider(&request.provider)?;
        let ProviderKind::External { start } = &provider.kind else {
            return Err(SubagentError::new(
                "UNSUPPORTED_CAPABILITY",
                "external start requires an external backend".to_owned(),
            ));
        };
        self.assert_capabilities(&provider, request)?;
        if !self.seams.agent_live(&request.parent_id).unwrap_or(false) {
            return Err(SubagentError::unauthorized(
                "subagent creation requires the exact live parent agent",
            ));
        }
        let cwd = self.resolve_directory(&request.parent_id, request.cwd.as_deref())?;
        let run = start(serde_json::json!({
            "parent": request.parent_id,
            "prompt": request.prompt,
            "cwd": cwd,
            "label": request.label,
        }))?;
        let child_id = uuid::Uuid::new_v4().to_string();
        let pool = self.pool_for(&request.parent_id);
        pool.reserve(self.config.max_active)?;
        let activation = Activation {
            child_id: child_id.clone(),
            provider: provider.name.clone(),
            parent_id: request.parent_id.clone(),
            ancestry: self.lineage_of(&request.parent_id),
            pool,
            resource: Resource::External {
                run,
                stopped: false,
            },
            announced: false,
            settled: false,
            owned_children: std::collections::BTreeSet::new(),
        };
        let parent_live = self.seams.agent_live(&request.parent_id).unwrap_or(false);
        if !parent_live {
            return Err(SubagentError::unauthorized(
                "subagent parent is no longer live",
            ));
        }
        self.resident
            .lock()
            .expect("resident lock")
            .insert(child_id.clone(), activation);
        // The external catalog row (dsh `establishExternalCatalogChild`).
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let row = serde_json::json!({
            "version": crate::catalog::EXTERNAL_CATALOG_VERSION,
            "childId": child_id,
            "childCreatedAt": u64::try_from(created_at).unwrap_or(0),
            "mode": "external",
            "label": request.label,
        });
        let _ = self
            .seams
            .session_append(&request.parent_id, crate::wire::CATALOG_EVENT, &row);
        if let Some(current) = self
            .resident
            .lock()
            .expect("resident lock")
            .get_mut(&child_id)
        {
            current.announced = true;
        }
        self.seams.emit_lifecycle(
            crate::wire::EVENT_START,
            &serde_json::json!({
                "runId": uuid::Uuid::new_v4().to_string(),
                "provider": provider.name,
                "id": child_id,
                "local": false,
            }),
        );
        Ok(StartReceipt {
            child_id,
            message_id: None,
        })
    }

    /// Interrupts one live child's current execution under the supplied
    /// authority (dsh `SubagentManager.interrupt`): fire-and-return, the
    /// four unauthorized shapes refuse before any signal.
    ///
    /// # Errors
    ///
    /// The authority refusal (an absent target is an accepted no-op).
    pub fn interrupt(
        &self,
        target_session_id: &str,
        authority: &InterruptAuthority,
    ) -> Result<(), SubagentError> {
        if let InterruptAuthority::Ancestor { agent } = authority {
            if !self.seams.agent_live(agent).unwrap_or(false) {
                return Err(SubagentError::unauthorized(format!(
                    "interrupting \"{target_session_id}\" requires the exact live ancestor agent"
                )));
            }
            if agent == target_session_id {
                return Err(SubagentError::unauthorized(format!(
                    "agent \"{agent}\" cannot interrupt itself"
                )));
            }
        }
        let activation = {
            let resident = self.resident.lock().expect("resident lock");
            resident.get(target_session_id).cloned()
        };
        let Some(activation) = activation else {
            return Ok(());
        };
        match authority {
            InterruptAuthority::User { parent_session_id } => {
                if activation.parent_id != *parent_session_id {
                    return Err(SubagentError::unauthorized(format!(
                        "subagent \"{target_session_id}\" belongs to another parent session"
                    )));
                }
            }
            InterruptAuthority::Ancestor { agent } => {
                if !activation.ancestry.iter().any(|id| id == agent) {
                    return Err(SubagentError::unauthorized(format!(
                        "subagent \"{target_session_id}\" is not a live descendant of agent \"{agent}\""
                    )));
                }
            }
        }
        self.interrupt_activation(&activation, &authority.cancel_kind());
        Ok(())
    }

    /// Steers one model-authored message to the sender's direct parent
    /// or direct continuable child (dsh `sendMessage`).
    ///
    /// # Errors
    ///
    /// The adjacency, authorization, or admission refusal.
    pub fn send_message(
        &self,
        sender: &str,
        target_id: &str,
        content: &[serde_json::Value],
    ) -> Result<String, SubagentError> {
        if !self.seams.agent_live(sender).unwrap_or(false) {
            return Err(SubagentError::unauthorized(
                "message delivery requires the exact live sender agent",
            ));
        }
        let sender_activation = {
            let resident = self.resident.lock().expect("resident lock");
            resident.get(sender).cloned()
        };
        if let Some(activation) = sender_activation {
            if activation.parent_id == target_id {
                return self.send_to_parent(&activation, sender, content);
            }
        }
        let parent_of_sender = self
            .parent_of(sender)
            .map_err(|detail| SubagentError::new("PARENT_UNAVAILABLE", detail))?;
        if parent_of_sender.as_deref() == Some(target_id) {
            return Err(SubagentError::unauthorized(format!(
                "agent \"{sender}\" is not a resident continuable child and cannot send to parent \"{target_id}\""
            )));
        }
        self.deliver_to_child(sender, target_id, content, Delivery::Steer)
    }

    /// The headless runner's non-blocking descendant probe (dsh
    /// `waitForChildren` reduced to the decision-46 carrier): reports
    /// whether any descendant of the session is still progressing.
    #[must_use]
    pub fn descendants_progressing(&self, session_id: &str) -> bool {
        let resident = self.resident.lock().expect("resident lock");
        resident.values().any(|activation| {
            activation.child_id != session_id
                && activation.ancestry.iter().any(|id| id == session_id)
                && match &activation.resource {
                    Resource::Local { .. } => {
                        self.seams
                            .agent_status(&activation.child_id)
                            .unwrap_or("idle")
                            == "running"
                    }
                    Resource::External { run, stopped } => !*stopped && !is_terminal(&run.result),
                }
        })
    }

    /// The natural-settlement entry the status listener calls (dsh
    /// `watchLocalSettlement`'s settle decision): re-checks under the
    /// per-child lock, captures the result, delivers the notice, and
    /// releases the residency epoch.
    pub fn settle_child(&self, child_id: &str) {
        let activation = {
            let resident = self.resident.lock().expect("resident lock");
            resident.get(child_id).cloned()
        };
        let Some(activation) = activation else {
            return;
        };
        if activation.settled {
            return;
        }
        let child_mutex = self.child_lock(child_id);
        let _guard = child_mutex.lock().expect("child lock");
        // Re-read under the lock: a delivery may have re-armed the child.
        let activation = {
            let resident = self.resident.lock().expect("resident lock");
            let Some(current) = resident.get(child_id) else {
                return;
            };
            if current.settled
                || !current.owned_children.is_empty()
                || self.seams.agent_status(child_id).unwrap_or("running") != "idle"
            {
                return;
            }
            current.clone()
        };
        self.finish_settlement(&activation);
    }

    /// Marks one activation settled and delivers the account (dsh
    /// `finishDisposal` + `notifySettlement` + `observer.settle`).
    fn finish_settlement(&self, activation: &Activation) {
        let result = self.capture_result(activation);
        let _ = self.seams.session_flush(&activation.child_id);
        {
            let mut resident = self.resident.lock().expect("resident lock");
            if let Some(current) = resident.get_mut(&activation.child_id) {
                current.settled = true;
            }
        }
        if activation.announced {
            self.deliver_settlement(activation, &result);
        }
        activation.pool.release();
        {
            let mut resident = self.resident.lock().expect("resident lock");
            resident.remove(&activation.child_id);
        }
        let local = matches!(activation.resource, Resource::Local { .. });
        self.seams.emit_lifecycle(
            crate::wire::EVENT_END,
            &serde_json::json!({
                "runId": uuid::Uuid::new_v4().to_string(),
                "provider": activation.provider,
                "id": activation.child_id,
                "local": local,
                "stopReason": result.stop_reason.as_str(),
                "lastAssistantMessage": result.output,
            }),
        );
    }

    /// Delivers the settlement notice to the durable direct parent (dsh
    /// `notifySettlement`): a queue turn for an idle parent, a steer at
    /// the nearest step for a running one.
    fn deliver_settlement(&self, activation: &Activation, result: &crate::domain::SubagentResult) {
        let parent_live = self
            .seams
            .agent_live(&activation.parent_id)
            .unwrap_or(false);
        if !parent_live {
            return;
        }
        let summary =
            crate::domain::settlement_summary(&activation.child_id, result.stop_reason, true);
        let blocks = crate::domain::settlement_notice_blocks(&activation.child_id, result, true);
        let message = serde_json::json!({
            "role": "user",
            "id": uuid::Uuid::new_v4().to_string(),
            "content": blocks,
            "source": crate::domain::settlement_source(&summary, &activation.child_id),
        });
        let status = self
            .seams
            .agent_status(&activation.parent_id)
            .unwrap_or("idle");
        let delivered = if status == "idle" {
            self.seams.agent_followup(&activation.parent_id, &message)
        } else {
            self.seams.agent_steer(&activation.parent_id, &message)
        };
        if delivered.is_err() {
            // The fallback injection is best-effort (dsh
            // `notifySettlement` catches and logs the delivery
            // failure; the settlement account is already durable).
            let _ = self.seams.agent_inject(&activation.parent_id, &message);
        }
    }

    /// Captures the local run result from the child log suffix (dsh
    /// `captureLocalResult`): the final assistant output, the epoch stop
    /// reason, and the committed structured value.
    fn capture_result(&self, activation: &Activation) -> crate::domain::SubagentResult {
        let (output_start, structured, failed) = match &activation.resource {
            Resource::Local {
                output_start,
                structured,
                failed,
            } => (output_start, structured, failed),
            Resource::External { run, .. } => return run.result.clone(),
        };
        let observation = self
            .seams
            .session_read(&activation.child_id)
            .unwrap_or_else(|_| Observation {
                source: "empty".to_owned(),
                header: serde_json::Value::Null,
                inherited_event_count: 0,
                events: Vec::new(),
            });
        let suffix_start = usize::try_from(*output_start).unwrap_or(0);
        let suffix: Vec<serde_json::Value> = observation
            .events
            .iter()
            .skip(suffix_start)
            .cloned()
            .collect();
        let output = crate::assistant_output::final_assistant_output(&suffix).unwrap_or_default();
        let stop_reason = if *failed {
            crate::domain::StopReason::Error
        } else {
            epoch_stop_reason(&suffix)
        };
        let structured_value = structured
            .as_ref()
            .and_then(|runtime| runtime.lock().expect("structured lock").captured().cloned());
        match (structured.as_ref(), structured_value) {
            (Some(_), Some(value)) => crate::domain::SubagentResult {
                output,
                stop_reason,
                structured: Some(value),
            },
            (Some(_), None) if stop_reason == crate::domain::StopReason::Completed => {
                crate::domain::SubagentResult {
                    output,
                    stop_reason: crate::domain::StopReason::Error,
                    structured: None,
                }
            }
            _ => crate::domain::SubagentResult {
                output,
                stop_reason,
                structured: None,
            },
        }
    }

    /// Marks one live failure at the driver's error boundary (dsh
    /// `observeLocalFailure`): a later committed turn ending clears it.
    pub fn observe_error(&self, session_id: &str) {
        let mut resident = self.resident.lock().expect("resident lock");
        if let Some(activation) = resident.get_mut(session_id) {
            if let Resource::Local { failed, .. } = &mut activation.resource {
                *failed = true;
            }
        }
    }

    /// Clears the failure marker when a later terminal event commits
    /// (dsh `session/event` turn/end listener arm).
    pub fn observe_turn_end(&self, session_id: &str) {
        let mut resident = self.resident.lock().expect("resident lock");
        if let Some(activation) = resident.get_mut(session_id) {
            if let Resource::Local { failed, .. } = &mut activation.resource {
                *failed = false;
            }
        }
    }

    /// Reads the direct-child catalog rows from the parent's durable
    /// log (dsh `listChildren`): the projection fold over the observed
    /// events, excluding fork-inherited facts.
    pub fn list_children(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<crate::catalog::CatalogEntry>, SubagentError> {
        let observation = self
            .seams
            .observe(parent_session_id)
            .map_err(|detail| SubagentError::new("SUBAGENT_CONTROL_QUERY_UNAVAILABLE", detail))?;
        let mut state: Option<serde_json::Value> = Some(serde_json::json!({
            "inheritedEventCount": observation.inherited_event_count,
        }));
        for event in &observation.events {
            state = Some(crate::catalog::fold(
                state,
                event,
                observation.inherited_event_count,
            ));
        }
        Ok(crate::catalog::entries(state.as_ref()))
    }

    /// Recursively lists reachable catalogs in stable pre-order (dsh
    /// `listDescendants`): external entries are leaves, unknown modes
    /// produce unsupported diagnostics, and unreadable catalogs stop
    /// only their branch.
    pub fn list_descendants(
        &self,
        root_session_id: &str,
    ) -> Result<Vec<crate::control::Row>, SubagentError> {
        let mut rows = Vec::new();
        let mut visited = std::collections::BTreeSet::new();
        let mut stack: Vec<(crate::catalog::CatalogEntry, String, u64)> = self
            .list_children(root_session_id)?
            .into_iter()
            .rev()
            .map(|entry| (entry, root_session_id.to_owned(), 1))
            .collect();
        visited.insert(root_session_id.to_owned());
        while let Some((entry, parent_id, depth)) = stack.pop() {
            if visited.contains(&entry.id) {
                continue;
            }
            visited.insert(entry.id.clone());
            let children: Vec<crate::catalog::CatalogEntry> = if entry.mode == "external" {
                Vec::new()
            } else {
                match self.list_children(&entry.id) {
                    Ok(children) => children,
                    Err(error) => {
                        let reason: &'static str =
                            if error.code == "SUBAGENT_CONTROL_QUERY_UNAVAILABLE" {
                                "unavailable"
                            } else {
                                "corrupt"
                            };
                        rows.push(crate::control::Row::Diagnostic(
                            crate::control::DiagnosticRow {
                                id: entry.id.clone(),
                                reason,
                                parent: Some(parent_id),
                                depth: Some(depth),
                            },
                        ));
                        continue;
                    }
                }
            };
            if entry.mode == "unknown" {
                rows.push(crate::control::Row::Diagnostic(
                    crate::control::DiagnosticRow {
                        id: entry.id.clone(),
                        reason: "unsupported",
                        parent: Some(parent_id),
                        depth: Some(depth),
                    },
                ));
            } else {
                let status = if self.seams.agent_live(&entry.id).unwrap_or(false)
                    && self.seams.agent_status(&entry.id).unwrap_or("idle") == "running"
                {
                    "running"
                } else {
                    "inactive"
                };
                rows.extend(crate::control::project_child(
                    &entry,
                    status,
                    Some((&parent_id, depth)),
                ));
            }
            for child in children.into_iter().rev() {
                stack.push((child, entry.id.clone(), depth + 1));
            }
        }
        Ok(rows)
    }

    /// Delivers one message to a resident child or cold-resumes the
    /// persisted direct child (dsh `deliverFollowup` + `coldResume`).
    fn deliver_to_child(
        &self,
        parent_id: &str,
        child_id: &str,
        content: &[serde_json::Value],
        delivery: Delivery,
    ) -> Result<String, SubagentError> {
        let activation = {
            let resident = self.resident.lock().expect("resident lock");
            resident.get(child_id).cloned()
        };
        match activation {
            Some(activation) => {
                if activation.parent_id != parent_id {
                    return Err(SubagentError::unauthorized(format!(
                        "subagent \"{child_id}\" belongs to another parent session"
                    )));
                }
                self.admit(&activation, parent_id, child_id, content, delivery, None)
            }
            None => self.cold_resume(parent_id, child_id, content, delivery),
        }
    }

    /// Cold-resumes one persisted direct child and submits the waiting
    /// turn (dsh `coldResume`): the descriptor supplies every
    /// reconstruction input; no provider is dispatched.
    fn cold_resume(
        &self,
        parent_id: &str,
        child_id: &str,
        content: &[serde_json::Value],
        delivery: Delivery,
    ) -> Result<String, SubagentError> {
        let observation = self.seams.observe(child_id).map_err(|_| {
            SubagentError::new(
                "NOT_RESUMABLE",
                format!("subagent \"{child_id}\" is unavailable"),
            )
        })?;
        let header_parent = observation
            .header
            .get("parentSession")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        if header_parent.as_deref() != Some(parent_id) {
            return Err(SubagentError::unauthorized(format!(
                "subagent \"{child_id}\" belongs to another parent session"
            )));
        }
        let own_events: Vec<serde_json::Value> = observation
            .events
            .iter()
            .skip(usize::try_from(observation.inherited_event_count).unwrap_or(0))
            .cloned()
            .collect();
        let descriptor = crate::descriptor::fold(&own_events)
            .map_err(|detail| SubagentError::new("NOT_RESUMABLE", detail))?;
        let Some(crate::descriptor::FoldedDescriptor::Continuable(descriptor)) = descriptor else {
            return Err(SubagentError::new(
                "NOT_RESUMABLE",
                format!(
                    "subagent \"{child_id}\" has no supported continuation state and cannot be resumed; choose a different target"
                ),
            ));
        };
        let independently_selected = own_events.iter().any(|event| {
            event.get("type").and_then(serde_json::Value::as_str)
                == Some(crate::wire::PRESET_SELECTED_EVENT)
        }) && !own_events.iter().any(|event| {
            event.get("type").and_then(serde_json::Value::as_str)
                == Some(crate::wire::DESCRIPTOR_EVENT)
        });
        let agent_preset = if independently_selected {
            let state = self
                .seams
                .projection_state(child_id, "agentPreset")
                .map_err(|_| {
                    SubagentError::new(
                        "NOT_RESUMABLE",
                        format!(
                            "subagent \"{child_id}\" has no agent preset projection for restoration"
                        ),
                    )
                })?;
            match state.and_then(|value| value.as_str().map(str::to_owned)) {
                Some(preset) => Some(preset),
                None => {
                    return Err(SubagentError::new(
                        "NOT_RESUMABLE",
                        format!(
                            "subagent \"{child_id}\" has no agent preset projection for restoration"
                        ),
                    ));
                }
            }
        } else {
            None
        };
        let route = ChildRoute {
            provider: descriptor.agent_provider.clone(),
            model: descriptor.agent_model.clone(),
            reasoning_effort: descriptor
                .agent_reasoning_effort
                .as_ref()
                .map(|effort| serde_json::json!(effort)),
            max_tokens: None,
        };
        let child_mutex = self.child_lock(child_id);
        let _guard = child_mutex.lock().expect("child lock");
        self.assert_child_id_available(child_id)?;
        let pool = self.pool_for(parent_id);
        pool.reserve(self.config.max_active)?;
        let cwd = observation
            .header
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default();
        let depth = observation
            .header
            .get("delegationDepth")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let activation = Activation {
            child_id: child_id.to_owned(),
            provider: descriptor.provider.clone(),
            parent_id: parent_id.to_owned(),
            ancestry: self.lineage_of(parent_id),
            pool,
            resource: Resource::Local {
                output_start: u64::try_from(observation.events.len()).unwrap_or(u64::MAX),
                structured: None,
                failed: false,
            },
            announced: false,
            settled: false,
            owned_children: std::collections::BTreeSet::new(),
        };
        self.seams
            .agent_resume(child_id, &route, None)
            .map_err(|detail| {
                SubagentError::new(
                    "NOT_RESUMABLE",
                    format!("subagent \"{child_id}\" is unavailable: {detail}"),
                )
            })?;
        let _ = (cwd, depth, agent_preset);
        self.resident
            .lock()
            .expect("resident lock")
            .insert(child_id.to_owned(), activation);
        self.admit_resident(child_id, parent_id, content, delivery)
    }

    /// The resident admission path shared by delivery and cold resume
    /// (dsh `submitAdmitted` + `announce`).
    fn admit_resident(
        &self,
        child_id: &str,
        parent_id: &str,
        content: &[serde_json::Value],
        delivery: Delivery,
    ) -> Result<String, SubagentError> {
        let activation = {
            let resident = self.resident.lock().expect("resident lock");
            resident.get(child_id).cloned()
        };
        let Some(activation) = activation else {
            return Err(SubagentError::new(
                "NOT_RESUMABLE",
                format!("subagent \"{child_id}\" is unavailable"),
            ));
        };
        self.admit(&activation, parent_id, child_id, content, delivery, None)
    }

    /// Admits one message across the final synchronous cutoff (dsh
    /// `submitAdmitted` + `deliver`): the structured input gate, the
    /// activation-kind gate, and the steer/followup fork.
    fn admit(
        &self,
        _activation: &Activation,
        parent_id: &str,
        child_id: &str,
        content: &[serde_json::Value],
        delivery: Delivery,
        source: Option<&serde_json::Value>,
    ) -> Result<String, SubagentError> {
        let child_mutex = self.child_lock(child_id);
        let _guard = child_mutex.lock().expect("child lock");
        let resident = self.resident.lock().expect("resident lock");
        let Some(current) = resident.get(child_id) else {
            return Err(SubagentError::new(
                "ACTIVATION_CLOSING",
                "subagent activation is being disposed; the message was not accepted",
            ));
        };
        if current.parent_id != parent_id {
            return Err(SubagentError::unauthorized(format!(
                "subagent \"{child_id}\" belongs to another parent session"
            )));
        }
        if let Resource::Local { structured, .. } = &current.resource {
            if let Some(runtime) = structured {
                if !runtime.lock().expect("structured lock").accepts_input() {
                    return Err(SubagentError::new(
                        "INPUT_CLOSED",
                        "subagent is submitting or has submitted its structured result; the message was not accepted",
                    ));
                }
            }
        } else {
            return Err(SubagentError::new(
                "NOT_CONTINUABLE",
                format!("subagent \"{child_id}\" does not accept follow-up input"),
            ));
        }
        drop(resident);
        let blocks: Vec<serde_json::Value> = match source {
            Some(_) => content.to_vec(),
            None => {
                let (blocks, source_value) =
                    crate::domain::agent_message_blocks(parent_id, content);
                let _ = source_value;
                blocks
            }
        };
        let message = serde_json::json!({
            "role": "user",
            "id": uuid::Uuid::new_v4().to_string(),
            "content": blocks,
            "source": source.cloned().unwrap_or_else(|| serde_json::json!({ "kind": "user" })),
        });
        match delivery {
            Delivery::Steer => self
                .seams
                .agent_steer(child_id, &message)
                .map_err(|detail| SubagentError::new("PARENT_UNAVAILABLE", detail))?,
            Delivery::Queue => self
                .seams
                .agent_followup(child_id, &message)
                .map_err(|detail| SubagentError::new("PARENT_UNAVAILABLE", detail))?,
        }
        let message_id = message
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(current) = self
            .resident
            .lock()
            .expect("resident lock")
            .get_mut(child_id)
        {
            current.announced = true;
        }
        self.seams.emit_lifecycle(
            crate::wire::EVENT_START,
            &serde_json::json!({
                "runId": uuid::Uuid::new_v4().to_string(),
                "provider": current_provider(self, child_id),
                "id": child_id,
                "local": true,
            }),
        );
        Ok(message_id)
    }

    /// Delivers one resident continuable child's message to its live
    /// direct parent (dsh `sendToParent`).
    fn send_to_parent(
        &self,
        activation: &Activation,
        sender: &str,
        content: &[serde_json::Value],
    ) -> Result<String, SubagentError> {
        if !self
            .seams
            .agent_live(&activation.parent_id)
            .unwrap_or(false)
        {
            return Err(SubagentError::new(
                "PARENT_UNAVAILABLE",
                "direct parent is not live; the message was not delivered",
            ));
        }
        let (blocks, source) = crate::domain::agent_message_blocks(sender, content);
        let message = serde_json::json!({
            "role": "user",
            "id": uuid::Uuid::new_v4().to_string(),
            "content": blocks,
            "source": source,
        });
        self.seams
            .agent_steer(&activation.parent_id, &message)
            .map_err(|_| {
                SubagentError::new(
                    "PARENT_UNAVAILABLE",
                    "direct parent is not live; the message was not delivered",
                )
            })?;
        Ok(message
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    /// Applies the cancel signal (dsh `interrupt`'s tail: local children
    /// keep their inbox; external executions stop permanently).
    fn interrupt_activation(&self, activation: &Activation, kind: &serde_json::Value) {
        match &activation.resource {
            Resource::Local { .. } => {
                let _ = self.seams.agent_cancel(&activation.child_id, kind, true);
            }
            Resource::External { run, stopped } => {
                (run.stop)(kind);
                let _ = stopped;
            }
        }
    }

    /// The initial prompt delivery (dsh `submitMaterialized`): a fresh
    /// child always queues its first turn as a user message; a
    /// continuable child with the adjacent messaging tool receives the
    /// return guidance appended.
    fn submit_initial(
        &self,
        child_id: &str,
        parent_id: &str,
        prompt: &[serde_json::Value],
        inherits_parent_context: bool,
    ) -> Result<String, SubagentError> {
        let guided = if inherits_parent_context {
            prompt.to_vec()
        } else {
            crate::domain::continuable_return_guidance(parent_id, prompt)
        };
        let message = serde_json::json!({
            "role": "user",
            "id": uuid::Uuid::new_v4().to_string(),
            "content": guided,
            "source": { "kind": "user" },
        });
        self.seams
            .agent_followup(child_id, &message)
            .map_err(|detail| SubagentError::new("PARENT_UNAVAILABLE", detail))?;
        if let Some(current) = self
            .resident
            .lock()
            .expect("resident lock")
            .get_mut(child_id)
        {
            current.announced = true;
        }
        self.seams.emit_lifecycle(
            crate::wire::EVENT_START,
            &serde_json::json!({
                "runId": uuid::Uuid::new_v4().to_string(),
                "provider": current_provider(self, child_id),
                "id": child_id,
                "local": true,
            }),
        );
        Ok(message
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    /// Creates the child session, appends the creation-window events,
    /// and attaches the driver (dsh `materializeTracked`'s local arm).
    fn materialize_local(
        &self,
        _activation: &Activation,
        create: &ChildCreate,
        descriptor: &serde_json::Value,
        delegated: &crate::domain::DelegatedPolicyOverrides,
        agent_preset: Option<&str>,
    ) -> Result<(), SubagentError> {
        self.seams.agent_create(create).map_err(|detail| {
            SubagentError::new("NOT_RESUMABLE", format!("child create failed: {detail}"))
        })?;
        if let Some(preset) = agent_preset {
            let _ = self.seams.session_append(
                &create.session_id,
                crate::wire::PRESET_SELECTED_EVENT,
                &serde_json::json!({ "agentPreset": preset }),
            );
        }
        let _ = self.seams.session_append(
            &create.session_id,
            crate::wire::DESCRIPTOR_EVENT,
            descriptor,
        );
        for (event_type, data) in crate::domain::delegated_policy_events(delegated) {
            let _ = self
                .seams
                .session_append(&create.session_id, event_type, &data);
        }
        let _ = self
            .seams
            .working_directory_set(&create.session_id, &create.cwd);
        Ok(())
    }

    /// Appends the durable direct-child fact to the parent log (dsh
    /// `establishCatalogChild`).
    fn establish_catalog(
        &self,
        parent_id: &str,
        child_id: &str,
        label: &str,
    ) -> Result<(), SubagentError> {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let row =
            crate::catalog::local_row(child_id, u64::try_from(created_at).unwrap_or(0), label);
        self.seams
            .session_append(parent_id, crate::wire::CATALOG_EVENT, &row)
            .map_err(|detail| SubagentError::new("CATALOG_UNAVAILABLE", detail))
    }

    /// Rejects one child identity already owned by a live activation,
    /// agent, or session (dsh `assertChildIdAvailable`).
    fn assert_child_id_available(&self, child_id: &str) -> Result<(), SubagentError> {
        let resident = self.resident.lock().expect("resident lock");
        if resident.contains_key(child_id)
            || self.seams.agent_live(child_id).unwrap_or(false)
            || self.seams.observe(child_id).is_ok()
        {
            return Err(SubagentError::duplicate_child(child_id));
        }
        Ok(())
    }

    /// Resolves the explicit or inherited directory (dsh
    /// `resolveDirectory`).
    fn resolve_directory(
        &self,
        parent_id: &str,
        requested: Option<&str>,
    ) -> Result<String, SubagentError> {
        match requested {
            Some(path) if std::path::Path::new(path).is_absolute() => Ok(path.to_owned()),
            Some(path) => {
                let base = self
                    .seams
                    .working_directory_ensure(parent_id)
                    .map_err(|detail| {
                        SubagentError::new("WORKING_DIRECTORY_UNAVAILABLE", detail)
                    })?;
                Ok(std::path::Path::new(&base)
                    .join(path)
                    .to_string_lossy()
                    .into_owned())
            }
            None => self
                .seams
                .working_directory_ensure(parent_id)
                .map_err(|detail| SubagentError::new("WORKING_DIRECTORY_UNAVAILABLE", detail)),
        }
    }

    /// Captures the delegation policy synchronously before the first
    /// await (dsh `captureDelegatedPolicyOverrides`).
    fn capture_delegated_policies(
        &self,
        parent_id: &str,
    ) -> crate::domain::DelegatedPolicyOverrides {
        // The dsh capture reads the parent's CURRENT preset identity and
        // its EXPLICIT sandbox override — never deployment defaults. The
        // parent's own log folds both exactly (the permission/preset and
        // sandbox/mode vocabulary the owners declare), so the capture
        // observes the durable facts without a projection-state coupling.
        let mut preset: Option<String> = None;
        let mut sandbox_mode: Option<String> = None;
        if let Ok(observation) = self.seams.observe(parent_id) {
            for event in &observation.events {
                let event_type = event.get("type").and_then(serde_json::Value::as_str);
                let data = event
                    .get("data")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                if event_type == Some("permission/preset") {
                    preset = data
                        .get("preset")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                } else if event_type == Some("sandbox/mode") {
                    sandbox_mode = data
                        .get("mode")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                }
            }
        }
        crate::domain::DelegatedPolicyOverrides {
            permission_preset: preset.filter(|name| name == "auto" || name == "danger-full-access"),
            sandbox_mode,
            approval_policy: if self.seams.approval_available().unwrap_or(false) {
                Some("never")
            } else {
                None
            },
        }
    }

    /// The exact ancestry from one agent upward (dsh `liveLineage`):
    /// the durable parent chain, nearest first, each link live.
    fn lineage_of(&self, session_id: &str) -> Vec<String> {
        let mut lineage = vec![session_id.to_owned()];
        let mut current = session_id.to_owned();
        for _ in 0..64 {
            let Some(parent) = self.parent_of(&current).ok().flatten() else {
                break;
            };
            if lineage.contains(&parent) {
                break;
            }
            lineage.push(parent.clone());
            current = parent;
        }
        lineage
    }

    /// The durable direct parent of one live or stored session.
    fn parent_of(&self, session_id: &str) -> Result<Option<String>, String> {
        let observation = self.seams.observe(session_id)?;
        Ok(observation
            .header
            .get("parentSession")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned))
    }

    /// The parent's persisted delegation depth (dsh
    /// `delegationDepthOf` over the live header).
    fn parent_depth(&self, parent_id: &str) -> Result<u64, SubagentError> {
        let observation = self
            .seams
            .observe(parent_id)
            .map_err(|detail| SubagentError::new("PARENT_UNAVAILABLE", detail))?;
        crate::depth::delegation_depth_of(
            observation
                .header
                .get("delegationDepth")
                .and_then(serde_json::Value::as_u64),
            None,
        )
        .map_err(|detail| SubagentError::new("PARENT_UNAVAILABLE", detail))
    }

    /// The parent's route as recorded by its latest request header
    /// (dsh `parentAgentOptionsForDelegation`).
    fn parent_route(&self, parent_id: &str) -> Result<ChildRoute, SubagentError> {
        let header = self
            .seams
            .request_header(parent_id)
            .map_err(|detail| SubagentError::new("PARENT_UNAVAILABLE", detail))?;
        let Some(header) = header else {
            return Ok(ChildRoute::default());
        };
        let config = header
            .get("config")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        Ok(ChildRoute {
            provider: config
                .get("provider")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            model: config
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            reasoning_effort: config.get("reasoningEffort").cloned(),
            max_tokens: None,
        })
    }

    /// The capacity pool one child shares: the resident parent's pool
    /// directly, otherwise the root's retained pool (dsh `rootPool`).
    fn pool_for(&self, parent_id: &str) -> Arc<ActivationPool> {
        {
            let resident = self.resident.lock().expect("resident lock");
            for activation in resident.values() {
                if activation.child_id == parent_id {
                    return Arc::clone(&activation.pool);
                }
            }
        }
        let mut roots = self.roots.lock().expect("roots lock");
        // Children of a non-resident root share the root's pool for the
        // manager's lifetime; settled roots release with their map entry.
        Arc::clone(roots.entry(parent_id.to_owned()).or_default())
    }

    /// Rejects the first requested capability the provider lacks (dsh
    /// `assertCapabilities`).
    fn assert_capabilities(
        &self,
        provider: &Provider,
        request: &StartRequest,
    ) -> Result<(), SubagentError> {
        if request.agent_preset.is_some()
            && (!provider.capabilities.agent_preset || provider.inherits_parent_context)
        {
            return Err(SubagentError::new(
                "UNSUPPORTED_CAPABILITY",
                format!(
                    "subagent provider \"{}\" does not support independent agent preset selection",
                    provider.name
                ),
            ));
        }
        let needs: [(bool, bool, &'static str); 3] = [
            (
                request.route.provider.is_some()
                    || request.route.model.is_some()
                    || request.route.reasoning_effort.is_some()
                    || request.route.max_tokens.is_some(),
                provider.capabilities.agent_options,
                "agentOptions",
            ),
            (
                request.output_schema.is_some(),
                provider.capabilities.output_schema,
                "outputSchema",
            ),
            (
                request.max_depth.is_some(),
                provider.capabilities.depth_limit,
                "depthLimit",
            ),
        ];
        for (needed, offered, capability) in needs {
            if needed && !offered {
                return Err(SubagentError::new(
                    "UNSUPPORTED_CAPABILITY",
                    format!(
                        "subagent provider \"{}\" does not support the \"{capability}\" capability",
                        provider.name
                    ),
                ));
            }
        }
        Ok(())
    }
}

impl Provider {
    /// One external provider registration (dsh `SubagentProvider` with
    /// `start` only; the M3 ACP bridge and the scripted manager tests
    /// register through this face).
    #[must_use]
    pub fn external(name: impl Into<String>, capabilities: Capabilities, start: StartFn) -> Self {
        Self {
            name: name.into(),
            capabilities,
            inherits_parent_context: false,
            kind: ProviderKind::External { start },
        }
    }

    /// External providers may advertise a static route default (dsh
    /// `agentRouteDefaults`); the in-process providers omit it.
    #[must_use]
    pub fn agent_route_defaults(&self) -> Option<()> {
        None
    }
}

/// Whether an external run result is already terminal (the join probe's
/// external arm).
fn is_terminal(result: &crate::domain::SubagentResult) -> bool {
    // The scripted external runs settle their result value once; the
    // `stopped` flag carries the liveness. This helper exists so the
    // probe reads intent, not state.
    let _ = result;
    false
}

/// The resident provider name for the lifecycle edge.
fn current_provider(manager: &Manager, child_id: &str) -> String {
    manager
        .resident
        .lock()
        .expect("resident lock")
        .get(child_id)
        .map_or_else(
            || "spawn".to_owned(),
            |activation| activation.provider.clone(),
        )
}

/// The delegation-depth policy from the tool configuration (dsh
/// `Config.maxDepth`: a non-negative cap or `'provider-managed'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfiguredDepth {
    /// Read the current host setting at each delegation.
    HostDefault,
    /// The explicit non-negative cap.
    Numeric(u64),
    /// The provider owns the recursion budget.
    ProviderManaged,
}

/// One start request (dsh `SubagentActivationSpec` minus the signal;
/// the fr carrier folds caller cancellation into the tool's execution
/// signal upstream).
#[derive(Debug, Clone)]
pub struct StartRequest {
    /// The registered backend to use.
    pub provider: String,
    /// The durable creation label.
    pub label: String,
    /// The optional reserved child id.
    pub child_id: Option<String>,
    /// The durable direct parent id.
    pub parent_id: String,
    /// The model-visible initial task.
    pub prompt: Vec<serde_json::Value>,
    /// The explicit absolute or parent-relative directory.
    pub cwd: Option<String>,
    /// The independent preset id.
    pub agent_preset: Option<String>,
    /// The per-child route overrides.
    pub route: ChildRoute,
    /// The optional structured-output schema.
    pub output_schema: Option<serde_json::Value>,
    /// The optional absolute recursion cap.
    pub max_depth: Option<u64>,
}

/// The accepted start receipt (dsh `SubagentActivation` + messageId).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartReceipt {
    /// The durable child session id.
    pub child_id: String,
    /// The accepted initial inbox message id (local children only).
    pub message_id: Option<String>,
}

/// The interrupt authority (dsh `SubagentInterruptAuthority`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterruptAuthority {
    /// The durable direct-parent address a human client presented.
    User {
        /// The claiming parent session id.
        parent_session_id: String,
    },
    /// The exact live ancestor agent authorizing the call.
    Ancestor {
        /// The caller's session id.
        agent: String,
    },
}

impl InterruptAuthority {
    /// The cancel-cause payload (dsh `cancel({ kind })`).
    #[must_use]
    pub fn cancel_kind(&self) -> serde_json::Value {
        match self {
            Self::User { .. } => serde_json::json!({ "kind": "user" }),
            Self::Ancestor { .. } => serde_json::json!({ "kind": "parent" }),
        }
    }
}

/// The inbox destination (dsh `SubagentDelivery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Admit at the nearest step boundary.
    Steer,
    /// Queue as a distinct turn.
    Queue,
}

/// The durable child creation metadata (dsh `childSessionMeta`).
#[must_use]
pub fn child_meta(
    parent_id: &str,
    child_depth: u64,
    is_seeded: bool,
    selected_preset: Option<&str>,
) -> serde_json::Value {
    let mut meta = serde_json::json!({
        "parentSession": parent_id,
        "isSeeded": is_seeded,
        "origin": "subagent",
        "delegationDepth": child_depth,
    });
    if let Some(preset) = selected_preset {
        meta["agentPreset"] = serde_json::json!(preset);
    }
    meta
}

/// Why this child's epoch ended, from its own event suffix (dsh
/// `epochStopReason`): the child's log is authoritative — teardown
/// succeeding says nothing about whether the model errored.
#[must_use]
pub fn epoch_stop_reason(events: &[serde_json::Value]) -> crate::domain::StopReason {
    let mut end: Option<&serde_json::Value> = None;
    let mut dropped_unrun = false;
    for event in events {
        let event_type = event.get("type").and_then(serde_json::Value::as_str);
        match event_type {
            Some("turn/end") => end = Some(event),
            Some("agent/error") => dropped_unrun = true,
            _ => {}
        }
    }
    let kind = end
        .and_then(|event| event.get("data"))
        .and_then(|data| data.get("reason"))
        .and_then(|reason| reason.get("kind"))
        .and_then(serde_json::Value::as_str);
    match kind {
        Some("max-tokens") => crate::domain::StopReason::MaxTokens,
        Some("aborted") | Some("interrupted") => crate::domain::StopReason::Aborted,
        Some("error") => crate::domain::StopReason::Error,
        Some("blocked") => crate::domain::StopReason::Refusal,
        Some("completed") | None => {
            if dropped_unrun {
                crate::domain::StopReason::Aborted
            } else {
                crate::domain::StopReason::Completed
            }
        }
        Some(_) => crate::domain::StopReason::Error,
    }
}

impl std::fmt::Debug for ExternalRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalRun").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("name", &self.name)
            .field("capabilities", &self.capabilities)
            .field("inherits_parent_context", &self.inherits_parent_context)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local { .. } => f.write_str("Local"),
            Self::External { .. } => f.write_str("External"),
        }
    }
}
