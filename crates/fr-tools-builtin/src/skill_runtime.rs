// Rust guideline compliant 2026-11-02
//! The skill capability seam (dsh `@deepseek-ai/dsh-skill`, 1:1): the
//! name grammar, the model-facing renderers, the candidate/summary/
//! definition vocabulary, and the registry.
//!
//! The registry merges provider catalogs, resolves the winning skill
//! for a name, and caches completed discoveries.
//!
//! This package owns the Service Definition role of the skill seam.
//! Concrete providers (the baked filesystem provider is
//! [`crate::skill_filesystem`]) decide where skills come from; the
//! registry only merges, ranks, validates, and loads. The tool bodies
//! call it in-process (Q93-A), so the seam is a plain Rust service and
//! the wire face is only the pre-step method the agent loop consumes.
//!
//! Structurally absent fr arms (Q69-B carrier judgment): the abort
//! signal rides nothing here (the tool pipeline owns deadlines), and
//! the scope-chain layering has no M1 assembly carrier (one global
//! layer; dsh's per-scope layers exist for agent presets fr has not
//! built). The dsh `skills/change` notification event has no M1
//! consumer (the catalog listener recomputes every step), so
//! invalidation is a bare revision bump.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// The default completed-catalog cache capacity (dsh
/// `DEFAULT_COLLECT_CACHE_ENTRIES`).
pub const DEFAULT_COLLECT_CACHE_ENTRIES: usize = 128;

/// The registry-owned provider label for runtime skill registrations
/// (dsh `RUNTIME_PROVIDER`).
pub const RUNTIME_PROVIDER: &str = "runtime";

/// The precedence rank of runtime skills within one layer (dsh
/// `RUNTIME_RANK`): project entries (100-200) outrank runtime, which
/// outranks user (400-500) and bundled (600) entries.
pub const RUNTIME_RANK: i64 = 250;

/// The standard precedence rank for packaged skill providers and local
/// bundled roots (dsh `BUNDLED_SKILL_RANK`).
pub const BUNDLED_SKILL_RANK: i64 = 600;

/// Whether a string is a valid kebab-case skill name (dsh
/// `isSkillName`).
#[must_use]
pub fn is_skill_name(name: &str) -> bool {
    // The grammar is plain ASCII, so the byte-level scan is the dsh
    // regex over code units.
    let mut segments = name.split('-');
    let first = segments.next().unwrap_or_default();
    if first.is_empty()
        || !first
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return false;
    }
    segments.all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

/// Escapes model-facing prose embedded inside skill attribute values so
/// provider-supplied text cannot break out of the attribute (dsh
/// `escapeAttr`).
#[must_use]
pub fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// Escapes model-facing prose embedded inside skill markup so
/// provider-supplied text cannot open or close framing tags (dsh
/// `escapeText`).
#[must_use]
pub fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Optional provider-specific base used by loaded skill bodies to
/// resolve relative resources (dsh `SkillResourceBase`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillResourceBase {
    /// Relative paths resolve against this directory.
    Directory {
        /// The absolute base directory.
        path: String,
    },
    /// Relative URLs resolve against this URL.
    Url {
        /// The base URL.
        url: String,
    },
    /// The provider describes resource access in prose.
    Opaque {
        /// The provider's resource-access description.
        description: String,
    },
}

/// Resolved invocation controls shared by skill discovery consumers
/// (dsh `SkillInvocationPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillInvocationPolicy {
    /// Whether model-facing catalogs and loaders include this skill.
    pub model_invocable: bool,
    /// Whether human-facing command catalogs and loaders include this
    /// skill.
    pub user_invocable: bool,
}

/// Invocation-neutral skill metadata returned by the registry list (dsh
/// `SkillSummary`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    /// Absolute instruction file path when supplied by the provider;
    /// absent for virtual skills.
    pub path: Option<String>,
    /// Kebab-case identifier used to address the skill.
    pub name: String,
    /// Short routing description shown by discovery consumers.
    pub description: String,
    /// Optional extra routing guidance.
    pub when_to_use: Option<String>,
    /// Resolved model and user invocation controls.
    pub invocation: SkillInvocationPolicy,
    /// Discovery source that produced this winning skill.
    pub source: String,
    /// Provider that owns this skill body.
    pub provider: String,
    /// Provider-specific base for relative resources.
    pub resource_base: Option<SkillResourceBase>,
}

/// The opaque provider-owned handle passed back to the provider's load
/// face (dsh `locator: unknown`; fr models the M1 closed set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillLocator {
    /// A runtime registration: the definition is its own locator (dsh
    /// `runtimeCandidate`).
    Runtime(Arc<SkillDefinition>),
    /// A local file locator (the filesystem provider's
    /// `{ path, directory }`).
    Local {
        /// The instruction file path.
        path: String,
        /// The directory relative resources resolve against.
        directory: String,
    },
}

/// Provider catalog entry used by the registry to merge and later load
/// skills (dsh `SkillCandidate`).
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "the metadata payload embeds serde_json::Value, so Eq is not derivable"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct SkillCandidate {
    /// Absolute instruction file path when supplied by the provider.
    pub path: Option<String>,
    /// Kebab-case identifier used to address the skill.
    pub name: String,
    /// Short routing description shown by discovery consumers.
    pub description: String,
    /// Optional extra routing guidance.
    pub when_to_use: Option<String>,
    /// Resolved model and user invocation controls.
    pub invocation: SkillInvocationPolicy,
    /// Discovery source that produced this winning skill.
    pub source: String,
    /// Provider that owns this skill body.
    pub provider: String,
    /// Provider-specific base for relative resources.
    pub resource_base: Option<SkillResourceBase>,
    /// Lower ranks win duplicate skill names before provider
    /// registration order is considered.
    pub rank: i64,
    /// Opaque provider-owned load handle.
    pub locator: SkillLocator,
    /// Parsed optional metadata object from provider-specific skill
    /// frontmatter.
    pub metadata: Option<Value>,
}

/// Complete parsed skill definition, including the body (dsh
/// `SkillDefinition`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDefinition {
    /// Absolute instruction file path when supplied by the provider.
    pub path: Option<String>,
    /// Kebab-case identifier used to address the skill.
    pub name: String,
    /// Short routing description shown by discovery consumers.
    pub description: String,
    /// Optional extra routing guidance.
    pub when_to_use: Option<String>,
    /// Resolved model and user invocation controls.
    pub invocation: SkillInvocationPolicy,
    /// Discovery source that produced this winning skill.
    pub source: String,
    /// Provider that owns this skill body.
    pub provider: String,
    /// Provider-specific base for relative resources.
    pub resource_base: Option<SkillResourceBase>,
    /// Markdown instruction body after any provider-specific metadata
    /// removal.
    pub content: String,
    /// Parsed optional metadata object from frontmatter.
    pub metadata: Option<Value>,
}

impl SkillDefinition {
    /// Projects the invocation-neutral summary (dsh `toSummary`).
    #[must_use]
    pub fn to_summary(&self) -> SkillSummary {
        SkillSummary {
            path: self.path.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            when_to_use: self.when_to_use.clone(),
            invocation: self.invocation,
            source: self.source.clone(),
            provider: self.provider.clone(),
            resource_base: self.resource_base.clone(),
        }
    }
}

impl From<&SkillCandidate> for SkillSummary {
    fn from(candidate: &SkillCandidate) -> Self {
        Self {
            path: candidate.path.clone(),
            name: candidate.name.clone(),
            description: candidate.description.clone(),
            when_to_use: candidate.when_to_use.clone(),
            invocation: candidate.invocation,
            source: candidate.source.clone(),
            provider: candidate.provider.clone(),
            resource_base: candidate.resource_base.clone(),
        }
    }
}

/// Whether a skill may be advertised to and loaded by a model (dsh
/// `isModelInvocable`).
#[must_use]
pub const fn is_model_invocable(skill: &SkillSummary) -> bool {
    skill.invocation.model_invocable
}

/// Whether a skill may be advertised to and loaded by a human-facing
/// command (dsh `isUserInvocable`).
#[must_use]
pub const fn is_user_invocable(skill: &SkillSummary) -> bool {
    skill.invocation.user_invocable
}

/// Renders the resource hint lines of one loaded skill (dsh
/// `renderResourceHint`): the four arms' texts are verbatim.
#[must_use]
fn render_resource_hint(provider: &str, base: Option<&SkillResourceBase>) -> Vec<String> {
    let Some(base) = base else {
        return vec![
            format!(
                "Resources for this skill are managed by provider \"{}\".",
                escape_text(provider)
            ),
            "Load referenced resources only as needed.".to_owned(),
        ];
    };
    match base {
        SkillResourceBase::Directory { path } => vec![
            format!("Base directory for this skill: {}", escape_text(path)),
            "Resolve relative paths mentioned by this skill against the base directory before \
using them. Load referenced resources only as needed."
                .to_owned(),
        ],
        SkillResourceBase::Url { url } => vec![
            format!("Base URL for this skill: {}", escape_text(url)),
            "Resolve relative URLs mentioned by this skill against the base URL before using \
them. Load referenced resources only as needed."
                .to_owned(),
        ],
        SkillResourceBase::Opaque { description } => vec![
            format!("Resources for this skill: {}", escape_text(description)),
            "Load referenced resources only as needed.".to_owned(),
        ],
    }
}

/// Renders one loaded skill for the model (dsh `renderSkillContent`).
///
/// The output is shared verbatim by the `skill` tool result and the
/// user-explicit invocation injection, so the model sees one canonical
/// `<skill_content>` shape on both paths. The name rides an escaped
/// attribute; the body is embedded verbatim (skills are trusted local
/// content, and user-supplied invocation text stays outside this
/// wrapper).
#[must_use]
pub fn render_skill_content(
    name: &str,
    provider: &str,
    resource_base: Option<&SkillResourceBase>,
    content: &str,
) -> String {
    let mut lines = vec![
        format!("<skill_content name=\"{}\">", escape_attr(name)),
        "<skill_resources>".to_owned(),
    ];
    lines.extend(render_resource_hint(provider, resource_base));
    lines.push("</skill_resources>".to_owned());
    lines.push(String::new());
    lines.push("<skill_instructions>".to_owned());
    lines.push(content.to_owned());
    lines.push("</skill_instructions>".to_owned());
    lines.push("</skill_content>".to_owned());
    lines.join("\n")
}

/// Caller context for cwd-sensitive provider work (dsh
/// `SkillLookupOptions` minus the fr-absent abort signal).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillLookupOptions {
    /// Workspace selector for the current lookup.
    pub cwd: Option<String>,
}

/// The provider output shape (dsh the array | observation union): a
/// complete-array shorthand, or an explicit observation when usable
/// candidates came from incomplete discovery.
#[derive(Debug, Clone)]
pub enum SkillProviderOutput {
    /// Every candidate, discovery complete.
    Complete(Vec<SkillCandidate>),
    /// Usable candidates from incomplete discovery; the registry keeps
    /// the rows but never caches the observation.
    Observation {
        /// The candidates available so far.
        candidates: Vec<SkillCandidate>,
        /// Whether discovery completed.
        complete: bool,
    },
}

/// Provider interface for one source of skills (dsh `SkillProvider`;
/// the fr face is synchronous — the filesystem provider runs on the
/// caller's worker thread, Q69-B).
pub trait SkillProvider: Send + Sync {
    /// Unique provider name in the registry.
    fn name(&self) -> &str;
    /// Lists available skill candidates for the current lookup context.
    ///
    /// # Errors
    ///
    /// The provider's own discovery failure (the registry contains it:
    /// the provider is skipped with a warning and the observation is
    /// not cached).
    fn list(&self, options: &SkillLookupOptions) -> Result<SkillProviderOutput, String>;
    /// Loads a complete skill body for a previously listed candidate.
    ///
    /// `Ok(None)` is the dsh `undefined` — the skill is no longer
    /// loadable (the file disappeared), not a failure.
    ///
    /// # Errors
    ///
    /// The provider's load failure family.
    fn get(
        &self,
        candidate: &SkillCandidate,
        options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>, String>;
}

/// Registration-scoped invalidation capability borrowed by one provider
/// (dsh `SkillProviderControl` minus the fr-absent abort signal).
pub struct SkillProviderControl<'a> {
    /// Invalidate completed catalogs and notify consumers.
    invalidate: Box<dyn Fn() + Send + Sync + 'a>,
}

impl SkillProviderControl<'_> {
    /// Invalidates completed catalogs (dsh `control.invalidate`).
    pub fn invalidate(&self) {
        (self.invalidate)();
    }
}

/// One provider registration retained by the layer (dsh
/// `RegisteredProvider`).
struct RegisteredProvider {
    provider: Arc<dyn SkillProvider>,
    /// Service-wide monotonic registration order, the within-layer rank
    /// tiebreak.
    order: u64,
}

/// One merged candidate plus its merge metadata (dsh
/// `IndexedCandidate`).
#[derive(Clone)]
struct IndexedCandidate {
    candidate: SkillCandidate,
    provider: Arc<dyn SkillProvider>,
    provider_order: i64,
    local_order: u64,
}

/// The single-layer skill registry (dsh `SkillRegistry` over one M1
/// global layer).
///
/// Providers and runtime skills register through the plugin apply;
/// reads merge everything, rank-deciding duplicate names within the
/// layer and registration order breaking remaining ties.
///
/// The collect cache and revision match the dsh semantics: a completed
/// discovery is cached under its cwd and the registry revision, an
/// invalidation bumps the revision and clears the cache, and a read
/// that observes a concurrent revision change retries once before
/// returning an uncached result.
pub struct SkillRegistry {
    inner: Mutex<RegistryInner>,
}

struct RegistryInner {
    /// Providers in registration order; names are unique (the
    /// duplicate-name refusal is the dsh `NamedEntries` error).
    providers: Vec<RegisteredProvider>,
    /// Runtime skills in registration order (first same-name
    /// registration wins).
    runtime: Vec<Arc<SkillDefinition>>,
    /// Completed discoveries in insertion order (the dsh Map LRU: no
    /// move-on-read, the oldest insertion evicts first).
    cache: Vec<(String, HashMap<String, IndexedCandidate>)>,
    revision: u64,
    next_provider_order: u64,
    collect_cache_max_entries: usize,
}

/// The registered-provider borrow handed to one `register_provider`
/// call (the dsh closure receives the registry's live state through
/// the guard; fr hands the provider list slice).
pub struct RegistrationGuard<'a> {
    providers: &'a mut Vec<RegisteredProvider>,
}

impl RegistrationGuard<'_> {
    /// Inserts one provider registration (the dsh `NamedEntries`
    /// insert with its duplicate-name refusal verbatim).
    fn insert(&mut self, name: &str, registration: RegisteredProvider) -> Result<UndoKind, String> {
        if self
            .providers
            .iter()
            .any(|registered| registered.provider.name() == name)
        {
            return Err(format!(
                "a skill provider named \"{name}\" is already registered"
            ));
        }
        let undo = UndoKind::Provider(Arc::clone(&registration.provider));
        self.providers.push(registration);
        Ok(undo)
    }
}

/// The reverse of one registration, derived from the operation record
/// (dsh the Cordis effect disposer): the provider reversal matches by
/// identity, the runtime reversal by name.
enum UndoKind {
    /// Removes the provider row owning the recorded provider object.
    Provider(Arc<dyn SkillProvider>),
    /// Removes the runtime table row with the recorded name.
    Runtime(String),
}

impl UndoKind {
    /// Applies the reversal against the live state.
    fn apply(self, inner: &mut RegistryInner) {
        match self {
            Self::Provider(provider) => {
                inner
                    .providers
                    .retain(|registered| !Arc::ptr_eq(&registered.provider, &provider));
            }
            Self::Runtime(name) => {
                inner.runtime.retain(|definition| definition.name != name);
            }
        }
    }
}

impl SkillRegistry {
    /// Assembles the registry over the resolved cache capacity (dsh
    /// the Config-validated constructor).
    ///
    /// # Errors
    ///
    /// `collect-cache-max-entries` below one (the dsh config
    /// validation, text verbatim).
    pub fn new(collect_cache_max_entries: usize) -> Result<Self, String> {
        assert_positive_integer("collectCacheMaxEntries", collect_cache_max_entries)?;
        Ok(Self {
            inner: Mutex::new(RegistryInner {
                providers: Vec::new(),
                runtime: Vec::new(),
                cache: Vec::new(),
                revision: 0,
                next_provider_order: 0,
                collect_cache_max_entries,
            }),
        })
    }

    /// Registers one borrowed same-process provider (dsh
    /// `registerProvider`): the factory receives the invalidation
    /// control, the duplicate-name refusal runs at insert, and the
    /// returned closure unregisters.
    ///
    /// # Errors
    ///
    /// The factory's own failure, or a duplicate provider name.
    ///
    /// # Panics
    ///
    /// A poisoned registry lock.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the insert must observe the same lock window as the order bookkeeping; a tighter scope would let a duplicate name race the slot"
    )]
    pub fn register_provider(
        &self,
        create: impl FnOnce(&SkillProviderControl<'_>) -> Result<Box<dyn SkillProvider>, String>,
    ) -> Result<Box<dyn FnOnce() + Send + '_>, String> {
        let control = SkillProviderControl {
            invalidate: Box::new(|| {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.invalidate();
                }
            }),
        };
        let provider: Arc<dyn SkillProvider> = Arc::from(create(&control)?);
        let name = provider.name().to_owned();
        if name == RUNTIME_PROVIDER {
            return Err(format!(
                "\"{RUNTIME_PROVIDER}\" is reserved for runtime skill registrations"
            ));
        }
        let undo = {
            let mut inner = self.inner.lock().expect("registry lock");
            let order = inner.next_provider_order;
            inner.next_provider_order += 1;
            let mut guard = RegistrationGuard {
                providers: &mut inner.providers,
            };
            guard.insert(
                name.as_str(),
                RegisteredProvider {
                    provider: Arc::clone(&provider),
                    order,
                },
            )?
        };
        Ok(Box::new(move || {
            let Ok(lock) = self.inner.lock() else {
                return;
            };
            let mut inner = lock;
            undo.apply(&mut inner);
            inner.invalidate();
        }))
    }

    /// Registers a runtime skill into the layer (dsh `register`):
    /// same-name runtime entries are first-wins; a duplicate logs the
    /// dsh warning and returns a no-op disposer so it cannot remove the
    /// winner.
    ///
    /// # Errors
    ///
    /// The runtime-skill validation texts verbatim.
    ///
    /// # Panics
    ///
    /// A poisoned registry lock.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the first-wins probe and the insert share one lock window; a tighter scope would let a duplicate registration race the winner"
    )]
    pub fn register(
        &self,
        skill: SkillDefinition,
    ) -> Result<Box<dyn FnOnce() + Send + '_>, String> {
        validate_runtime_skill(&skill)?;
        let undo = {
            let mut inner = self.inner.lock().expect("registry lock");
            if inner
                .runtime
                .iter()
                .any(|existing| existing.name == skill.name)
            {
                eprintln!(
                    "runtime skill \"{}\" ignored because it is already registered",
                    skill.name
                );
                None
            } else {
                let definition = Arc::new(skill);
                inner.runtime.push(Arc::clone(&definition));
                Some(UndoKind::Runtime(definition.name.clone()))
            }
        };
        let Some(undo) = undo else {
            return Ok(Box::new(|| {}));
        };
        Ok(Box::new(move || {
            if let Ok(mut inner) = self.inner.lock() {
                undo.apply(&mut inner);
                inner.invalidate();
            }
        }))
    }

    /// Lists invocation-neutral skill summaries for a workspace (dsh
    /// `list`).
    ///
    /// # Errors
    ///
    /// The collect refusal family (provider data validation).
    pub fn list(&self, options: &SkillLookupOptions) -> Result<Vec<SkillSummary>, String> {
        Ok(self.snapshot(options)?.skills)
    }

    /// Observes the current invocation-neutral catalog and whether
    /// discovery completed within a stable revision (dsh `snapshot`).
    /// Incomplete observations are never cached.
    ///
    /// # Errors
    ///
    /// The collect refusal family.
    pub fn snapshot(&self, options: &SkillLookupOptions) -> Result<SkillCatalogSnapshot, String> {
        let collected = self.collect(options)?;
        let mut skills: Vec<SkillSummary> = collected
            .entries
            .values()
            .map(|entry| SkillSummary::from(&entry.candidate))
            .collect();
        skills.sort_by(compare_skill_summary);
        Ok(SkillCatalogSnapshot {
            skills,
            complete: collected.cacheable,
        })
    }
}

/// One catalog observation plus whether discovery completed within a
/// stable catalog revision (dsh `SkillCatalogSnapshot`).
pub struct SkillCatalogSnapshot {
    /// Sorted invocation-neutral summaries collected in this
    /// observation.
    pub skills: Vec<SkillSummary>,
    /// Whether every registered provider completed without a
    /// concurrent catalog revision.
    pub complete: bool,
}

/// One collect outcome: the merged candidates plus the cacheable probe.
struct CollectResult {
    entries: HashMap<String, IndexedCandidate>,
    cacheable: bool,
}

/// The registry-owned runtime provider (dsh `RUNTIME_SKILL_PROVIDER`):
/// `list()` contributes nothing (runtime skills ride the runtime table)
/// and `get()` returns the definition its candidate carries.
#[derive(Debug)]
struct RuntimeSkillProvider;

impl SkillProvider for RuntimeSkillProvider {
    fn name(&self) -> &str {
        RUNTIME_PROVIDER
    }

    fn list(&self, _options: &SkillLookupOptions) -> Result<SkillProviderOutput, String> {
        Ok(SkillProviderOutput::Complete(Vec::new()))
    }

    fn get(
        &self,
        candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>, String> {
        match &candidate.locator {
            SkillLocator::Runtime(definition) => Ok(Some(definition.as_ref().clone())),
            SkillLocator::Local { .. } => Ok(None),
        }
    }
}

/// The process-wide runtime provider identity (dsh the module
/// singleton).
fn runtime_skill_provider() -> Arc<dyn SkillProvider> {
    static RUNTIME: std::sync::OnceLock<Arc<dyn SkillProvider>> = std::sync::OnceLock::new();
    Arc::clone(RUNTIME.get_or_init(|| Arc::new(RuntimeSkillProvider) as Arc<dyn SkillProvider>))
}

/// The registered runtime skill's catalog candidate (dsh
/// `runtimeCandidate`): the runtime rank and the definition-as-locator
/// spread.
fn runtime_candidate(skill: &Arc<SkillDefinition>) -> SkillCandidate {
    SkillCandidate {
        path: skill.path.clone(),
        name: skill.name.clone(),
        description: skill.description.clone(),
        when_to_use: skill.when_to_use.clone(),
        invocation: skill.invocation,
        source: skill.source.clone(),
        provider: skill.provider.clone(),
        resource_base: skill.resource_base.clone(),
        rank: RUNTIME_RANK,
        locator: SkillLocator::Runtime(Arc::clone(skill)),
        metadata: skill.metadata.clone(),
    }
}

impl SkillRegistry {
    /// Loads and validates the winning candidate (dsh `get`): the name
    /// grammar gate, the collect, the provider load, and the post-load
    /// validation with the stale-definition invalidation.
    ///
    /// # Errors
    ///
    /// The load or validation refusal family.
    pub fn get(
        &self,
        name: &str,
        options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>, String> {
        if !is_skill_name(name) {
            return Ok(None);
        }
        let collected = self.collect(options)?;
        let Some(matched) = collected.entries.get(name) else {
            return Ok(None);
        };
        let provider = Arc::clone(&matched.provider);
        let candidate = matched.candidate.clone();
        let definition = provider.get(&candidate, options)?;
        let Some(definition) = definition else {
            return Ok(None);
        };
        validate_definition(&definition)?;
        if definition.name != candidate.name {
            self.invalidate_entry(matched);
            return Ok(None);
        }
        Ok(Some(definition))
    }

    /// Runs one collect: cache probe, fresh discovery, and the
    /// revision-retry ladder (dsh `collect`).
    ///
    /// # Errors
    ///
    /// The provider data validation refusals.
    fn collect(&self, options: &SkillLookupOptions) -> Result<CollectResult, String> {
        let mut attempt = 1;
        loop {
            let (revision, key) = {
                let inner = self.inner.lock().expect("registry lock");
                (inner.revision, inner.cache_key(options.cwd.as_deref()))
            };
            let cached = {
                let inner = self.inner.lock().expect("registry lock");
                inner
                    .cache
                    .iter()
                    .find(|(cached_key, _)| cached_key == &key)
                    .map(|(_, entries)| entries.clone())
            };
            if let Some(entries) = cached {
                return Ok(CollectResult {
                    entries,
                    cacheable: true,
                });
            }
            let result = self.collect_fresh(options)?;
            let stale = {
                let inner = self.inner.lock().expect("registry lock");
                inner.revision != revision
            };
            if stale {
                if attempt < MAX_COLLECT_ATTEMPTS {
                    attempt += 1;
                    continue;
                }
                return Ok(CollectResult {
                    entries: result.entries,
                    cacheable: false,
                });
            }
            if result.cacheable {
                let mut inner = self.inner.lock().expect("registry lock");
                // The revision re-check closes the dsh-impossible race
                // where an invalidation lands between the fresh
                // discovery and the cache insert.
                if inner.revision == revision {
                    inner.cache.push((key, result.entries.clone()));
                    if inner.cache.len() > inner.collect_cache_max_entries {
                        inner.cache.remove(0);
                    }
                }
            }
            return Ok(result);
        }
    }

    /// Discovers fresh: runtime rows first, then each provider in
    /// registration order; rank sorts the layer and the first same-name
    /// entry wins (dsh `collectFresh` over the one M1 layer). Provider
    /// discovery runs without the registry lock so a provider's own
    /// invalidation (the watcher) cannot deadlock the read.
    ///
    /// # Errors
    ///
    /// The provider data validation refusals.
    fn collect_fresh(&self, options: &SkillLookupOptions) -> Result<CollectResult, String> {
        let (providers, runtime) = {
            let inner = self.inner.lock().expect("registry lock");
            (
                inner
                    .providers
                    .iter()
                    .map(|registered| (Arc::clone(&registered.provider), registered.order))
                    .collect::<Vec<_>>(),
                inner.runtime.clone(),
            )
        };
        let mut candidates: Vec<IndexedCandidate> = Vec::new();
        let mut cacheable = true;
        for (runtime_order, definition) in runtime.iter().enumerate() {
            candidates.push(IndexedCandidate {
                candidate: runtime_candidate(definition),
                provider: runtime_skill_provider(),
                provider_order: -1,
                local_order: runtime_order as u64,
            });
        }
        for (provider, order) in providers {
            let output = match provider.list(options) {
                Ok(output) => output,
                Err(error) => {
                    cacheable = false;
                    eprintln!("skill provider \"{}\" skipped: {error}", provider.name());
                    continue;
                }
            };
            let (provider_candidates, complete) = match output {
                SkillProviderOutput::Complete(candidates) => (candidates, true),
                SkillProviderOutput::Observation {
                    candidates,
                    complete,
                } => (candidates, complete),
            };
            if !complete {
                cacheable = false;
            }
            for (local_order, candidate) in provider_candidates.into_iter().enumerate() {
                validate_candidate(&candidate, provider.name())?;
                candidates.push(IndexedCandidate {
                    candidate,
                    provider: Arc::clone(&provider),
                    provider_order: i64::try_from(order).unwrap_or(i64::MAX),
                    local_order: local_order as u64,
                });
            }
        }
        candidates.sort_by(compare_indexed_candidates);
        let mut entries = HashMap::new();
        for entry in candidates {
            let name = entry.candidate.name.clone();
            if entries.contains_key(&name) {
                eprintln!(
                    "skill \"{}\" from {} ignored because a higher-priority skill already exists",
                    name, entry.candidate.source
                );
                continue;
            }
            entries.insert(name, entry);
        }
        Ok(CollectResult { entries, cacheable })
    }

    /// Invalidates after a stale definition load, only while the exact
    /// provider registration that produced the entry is still live
    /// (dsh `invalidateEntry`).
    fn invalidate_entry(&self, entry: &IndexedCandidate) {
        let mut inner = self.inner.lock().expect("registry lock");
        if inner
            .providers
            .iter()
            .any(|registered| Arc::ptr_eq(&registered.provider, &entry.provider))
        {
            inner.invalidate();
        }
    }
}

impl RegistryInner {
    /// Bumps the revision and drops every completed catalog (dsh
    /// `invalidateCache` minus the fr-absent change notification).
    fn invalidate(&mut self) {
        self.revision += 1;
        self.cache.clear();
    }

    /// Builds the collect-cache key (dsh `collectCacheKey` minus the
    /// fr-absent scope chain; the cwd key is dropped when absent, the
    /// dsh `JSON.stringify` undefined-key shape).
    fn cache_key(&self, cwd: Option<&str>) -> String {
        let mut key = serde_json::Map::new();
        if let Some(cwd) = cwd {
            key.insert("cwd".to_owned(), serde_json::json!(cwd));
        }
        key.insert("revision".to_owned(), serde_json::json!(self.revision));
        serde_json::Value::Object(key).to_string()
    }
}

/// The fr-meaningful subset of the dsh candidate validation (dsh
/// `validateCandidate`): the type-shape arms are compiler-guaranteed by
/// the typed provider face, so the surviving checks guard the
/// cross-provider contract with the dsh texts verbatim.
///
/// # Errors
///
/// The invalid-name, empty-description, and provider-mismatch refusals.
fn validate_candidate(candidate: &SkillCandidate, provider_name: &str) -> Result<(), String> {
    if !is_skill_name(&candidate.name) {
        return Err(format!(
            "skill provider \"{provider_name}\" returned invalid skill name \"{}\"",
            candidate.name
        ));
    }
    if candidate.description.is_empty() {
        return Err(format!(
            "skill provider \"{provider_name}\" returned skill \"{}\" without a description",
            candidate.name
        ));
    }
    if candidate.provider != provider_name {
        return Err(format!(
            "skill provider \"{provider_name}\" returned skill \"{}\" for provider \"{}\"",
            candidate.name, candidate.provider
        ));
    }
    Ok(())
}

/// Validates one runtime registration (dsh `validateRuntimeSkill`; the
/// invocation type-shape arm is compiler-guaranteed).
///
/// # Errors
///
/// The invalid-name and empty-description refusals verbatim.
fn validate_runtime_skill(skill: &SkillDefinition) -> Result<(), String> {
    if !is_skill_name(&skill.name) {
        return Err(format!("invalid skill name \"{}\"", skill.name));
    }
    if skill.description.is_empty() {
        return Err(format!("skill \"{}\" requires a description", skill.name));
    }
    Ok(())
}

/// Validates one provider-loaded definition (dsh `validateDefinition`;
/// the type-shape arms are compiler-guaranteed).
///
/// # Errors
///
/// The invalid-name and empty-description refusals verbatim.
fn validate_definition(skill: &SkillDefinition) -> Result<(), String> {
    if !is_skill_name(&skill.name) {
        return Err(format!("loaded skill has invalid name \"{}\"", skill.name));
    }
    if skill.description.is_empty() {
        return Err(format!(
            "loaded skill \"{}\" requires a description",
            skill.name
        ));
    }
    Ok(())
}

/// The catalog sort (dsh `compareSkillSummary`): the dsh code-point
/// compare is the byte compare over the ASCII-only validated names.
fn compare_skill_summary(left: &SkillSummary, right: &SkillSummary) -> std::cmp::Ordering {
    left.name.cmp(&right.name)
}

/// The layer merge order (dsh `compareIndexedCandidates`): rank, then
/// provider registration order, then the provider's local order.
fn compare_indexed_candidates(
    left: &IndexedCandidate,
    right: &IndexedCandidate,
) -> std::cmp::Ordering {
    left.candidate
        .rank
        .cmp(&right.candidate.rank)
        .then(left.provider_order.cmp(&right.provider_order))
        .then(left.local_order.cmp(&right.local_order))
}

/// The config positive-integer refusal (dsh `assertPositiveInteger`
/// with the registry prefix).
///
/// # Errors
///
/// Below one (the text is verbatim).
fn assert_positive_integer(name: &str, value: usize) -> Result<(), String> {
    if value < 1 {
        return Err(format!(
            "skill: {name} must be an integer greater than or equal to 1"
        ));
    }
    Ok(())
}

/// The collect retry ceiling (dsh `MAX_COLLECT_ATTEMPTS`).
const MAX_COLLECT_ATTEMPTS: u32 = 2;

impl std::fmt::Debug for SkillProviderControl<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillProviderControl")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SkillRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillRegistry").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RegistrationGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationGuard").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SkillCatalogSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillCatalogSnapshot")
            .field("small", &self.skills.len())
            .field("complete", &self.complete)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scripted provider: a fixed output per read, a read counter
    /// for the cache probes, and the runtime passthrough for `get`.
    struct FakeProvider {
        name: String,
        output: Mutex<Result<SkillProviderOutput, String>>,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl FakeProvider {
        fn named(name: &str, candidates: Vec<SkillCandidate>) -> Self {
            Self {
                name: name.to_owned(),
                output: Mutex::new(Ok(SkillProviderOutput::Complete(candidates))),
                reads: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl SkillProvider for FakeProvider {
        fn name(&self) -> &str {
            &self.name
        }

        fn list(&self, _options: &SkillLookupOptions) -> Result<SkillProviderOutput, String> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.output.lock().expect("output").clone()
        }

        fn get(
            &self,
            candidate: &SkillCandidate,
            _options: &SkillLookupOptions,
        ) -> Result<Option<SkillDefinition>, String> {
            match &candidate.locator {
                SkillLocator::Runtime(definition) => Ok(Some(definition.as_ref().clone())),
                SkillLocator::Local { .. } => Ok(None),
            }
        }
    }

    /// Builds one local candidate (the filesystem provider's shape).
    fn local_candidate(name: &str, rank: i64, source: &str) -> SkillCandidate {
        SkillCandidate {
            path: Some(format!("/r/{name}/SKILL.md")),
            name: name.to_owned(),
            description: format!("the {name} skill"),
            when_to_use: None,
            invocation: SkillInvocationPolicy {
                model_invocable: true,
                user_invocable: true,
            },
            source: source.to_owned(),
            provider: "fake".to_owned(),
            resource_base: Some(SkillResourceBase::Directory {
                path: format!("/r/{name}"),
            }),
            rank,
            locator: SkillLocator::Local {
                path: format!("/r/{name}/SKILL.md"),
                directory: format!("/r/{name}"),
            },
            metadata: None,
        }
    }

    /// Builds one runtime definition.
    fn runtime_definition(name: &str) -> SkillDefinition {
        SkillDefinition {
            path: None,
            name: name.to_owned(),
            description: format!("runtime {name}"),
            when_to_use: None,
            invocation: SkillInvocationPolicy {
                model_invocable: true,
                user_invocable: true,
            },
            source: "custom".to_owned(),
            provider: RUNTIME_PROVIDER.to_owned(),
            resource_base: None,
            content: format!("body of {name}"),
            metadata: None,
        }
    }

    /// The names of one snapshot, in catalog order.
    fn names(skills: &[SkillSummary]) -> Vec<&str> {
        skills.iter().map(|skill| skill.name.as_str()).collect()
    }

    /// Assembles one registry over scripted providers (the read-side
    /// helper; the disposer lives as long as the registry).
    fn new_registry(providers: &[(&str, Vec<SkillCandidate>)]) -> SkillRegistry {
        let registry = SkillRegistry::new(DEFAULT_COLLECT_CACHE_ENTRIES).expect("registry");
        for (name, candidates) in providers {
            let provider = FakeProvider::named(name, candidates.clone());
            let _disposer = registry
                .register_provider(|_control| Ok(Box::new(provider) as Box<dyn SkillProvider>))
                .expect("register");
        }
        registry
    }

    #[test]
    fn name_grammar_pins_the_kebab_shape() {
        assert!(is_skill_name("a"));
        assert!(is_skill_name("abc-def01"));
        assert!(!is_skill_name(""));
        assert!(!is_skill_name("-a"));
        assert!(!is_skill_name("a-"));
        assert!(!is_skill_name("A"));
        assert!(!is_skill_name("a_b"));
        assert!(!is_skill_name("a/b"));
    }

    #[test]
    fn escapes_pin_the_markup_safety() {
        assert_eq!(escape_text("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(escape_attr("a\"b<c"), "a&quot;b&lt;c");
    }

    #[test]
    fn render_pins_the_four_resource_arms_verbatim() {
        let directory = render_skill_content(
            "pdf",
            "filesystem",
            Some(&SkillResourceBase::Directory {
                path: "/r/pdf".to_owned(),
            }),
            "body",
        );
        assert_eq!(
            directory,
            "<skill_content name=\"pdf\">\n<skill_resources>\nBase directory for this skill: /r/pdf\nResolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.\n</skill_resources>\n\n<skill_instructions>\nbody\n</skill_instructions>\n</skill_content>"
        );
    }

    #[test]
    fn registry_lists_sorted_and_complete() {
        let registry = new_registry(&[(
            "fake",
            vec![
                local_candidate("z-skill", 300, "custom"),
                local_candidate("a-skill", 300, "custom"),
            ],
        )]);
        let snapshot = registry
            .snapshot(&SkillLookupOptions { cwd: None })
            .expect("snapshot");
        assert_eq!(names(&snapshot.skills), ["a-skill", "z-skill"]);
        assert!(snapshot.complete);
    }

    #[test]
    fn rank_decides_duplicate_names_within_the_layer() {
        let registry = new_registry(&[(
            "fake",
            vec![
                local_candidate("dup", 400, "user-dsh"),
                local_candidate("dup", 100, "project-dsh"),
                local_candidate("solo", 600, "bundled"),
            ],
        )]);
        let snapshot = registry
            .snapshot(&SkillLookupOptions { cwd: None })
            .expect("snapshot");
        assert_eq!(names(&snapshot.skills), ["dup", "solo"]);
        assert_eq!(snapshot.skills[0].source, "project-dsh");
    }

    #[test]
    fn runtime_rank_sits_between_project_and_user_ranks() {
        let registry = new_registry(&[(
            "fake",
            vec![
                local_candidate("dup", 400, "user-dsh"),
                local_candidate("solo", 600, "bundled"),
            ],
        )]);
        let _dispose = registry
            .register(runtime_definition("dup"))
            .expect("register");
        let snapshot = registry
            .snapshot(&SkillLookupOptions { cwd: None })
            .expect("snapshot");
        // Runtime (250) beats the user rank 400; the project rank 100 would outrank it.
        assert_eq!(snapshot.skills[0].provider, "runtime");
    }
}
