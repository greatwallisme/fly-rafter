//! The replay-aware compaction pipeline (dsh compaction-basic/src/index.ts
//! `compactIfNeeded` port, plus the target helpers it shares with the
//! summarizer).
//!
//! Both triggers price the latest durable routed request envelope; the
//! overflow trigger bypasses the threshold and retained-tail policy so it
//! can force one useful balanced reduction, while the pressure trigger
//! resolves the routed model's capacity and checks its target-specific
//! threshold first. The model-free prune pass (dsh `toolResultPruner`)
//! is an optional sibling service: the dsh soft-get shape means an
//! absent pruner skips the pass, and the fr probe rides the same shape
//! (the pruner's own face lands with its batch).
//!
//! The transaction itself is [`crate::region::compact_surface_region`]
//! with the auto-path options (`current-turn` owner, whole-surface
//! stability); every pricing decision rides the injected meter.

use crate::config::{
    ResolvedCompactSpec, ResolvedConfig, TargetPressureConfigError, resolve_compact_spec,
    resolve_target_policy,
};
use crate::region::{
    CompactionError, RegionDeps, SessionView, TransactionOptions, assert_no_active_compaction,
    compact_surface_region, select_compactable_range,
};
use crate::summarizer::{CompactionFailure, SummarizationInput, SummaryResult};
use crate::types::{CompactionResult, CompactionTrigger};

/// One route's capacity facts (the dsh `ModelInfo` shape the pressure
/// trigger consumes: the context window plus the adapter's per-request
/// default output cap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCapacity {
    /// The adapter-declared full window, when the route declares one.
    pub context_window: Option<i64>,
    /// The per-request default output cap, when the adapter materializes
    /// one.
    pub default_max_tokens: Option<u64>,
}

/// The engine's seams (dsh `BasicCompactionEngine`'s member services).
///
/// The meter, the summarizer hook, the recovery hook, and the capacity
/// lookup: borrowed per pipeline run; the shell constructs them over the
/// wire faces.
pub struct EngineDeps<'a> {
    /// The effective token meter.
    pub meter: &'a dyn crate::region::MeterView,
    /// The summarizer hook — the engine's sole customization point.
    pub summarize:
        &'a mut dyn FnMut(&SummarizationInput) -> Result<SummaryResult, CompactionFailure>,
    /// The summary-error recovery hook; `true` re-prepares and retries.
    pub recover: &'a mut dyn FnMut(&CompactionFailure, &[fr_wire::SessionSeq]) -> bool,
    /// The routed model's capacity facts (the dsh
    /// `ctx.llm.resolveModelInfo` projection).
    pub model_capacity: &'a mut dyn FnMut(&str, &str) -> Result<ModelCapacity, String>,
}

impl std::fmt::Debug for EngineDeps<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineDeps").finish_non_exhaustive()
    }
}

/// The pipeline's failure face: the target-specific config rejection
/// (the pressure trigger's warned-once key rides it), the transaction's
/// own errors, and the plain diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// A target-specific pressure-config rejection.
    TargetPressure(TargetPressureConfigError),
    /// The compaction transaction failed.
    Region(CompactionError),
    /// Any other backend diagnostic, carried verbatim.
    Plain(String),
}

impl EngineError {
    /// The verbatim diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::TargetPressure(error) => error.message(),
            Self::Region(error) => match error {
                CompactionError::Manual(manual) => manual.message(),
                CompactionError::Failure(failure) => failure.message(),
            },
            Self::Plain(message) => message,
        }
    }

    /// The warning-suppression key when the failure is a
    /// target-specific pressure-config rejection (the dsh
    /// `warnedPressureConfigTargets` set).
    #[must_use]
    pub const fn target_key(&self) -> Option<&str> {
        match self {
            Self::TargetPressure(error) => Some(error.target_key.as_str()),
            _ => None,
        }
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for EngineError {}

/// Resolve the exact provider/model durably routed for the latest
/// request (dsh `routedTarget`): both spellings non-empty.
#[must_use]
pub fn routed_target(view: &dyn SessionView) -> Option<(String, String)> {
    let header = view.request_header()?;
    let config = &header.config;
    if config.provider.is_empty() || config.model.is_empty() {
        return None;
    }
    Some((config.provider.clone(), config.model.clone()))
}

/// Resolve the conversation target used to select an optional policy
/// override and the summarization fallback (dsh `conversationTarget`):
/// the routed pair first, then the declared agent route.
#[must_use]
pub fn conversation_target(
    view: &dyn SessionView,
    agent_route: Option<(&str, &str)>,
) -> Option<(String, String)> {
    if let Some(routed) = routed_target(view) {
        return Some(routed);
    }
    let (provider, model) = agent_route?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider.to_owned(), model.to_owned()))
}

/// Output tokens the routed request reserves (dsh
/// `reservedCompletionTokens`): the effective envelope's own cap wins,
/// else the adapter's per-request default; no declared cap reserves
/// nothing.
#[must_use]
pub fn reserved_completion_tokens(view: &dyn SessionView, default_max_tokens: Option<u64>) -> i64 {
    let configured = view
        .request_header()
        .map(|header| header.config.controls.max_tokens)
        .unwrap_or_default();
    configured.map_or_else(
        || i64::try_from(default_max_tokens.unwrap_or(0)).unwrap_or(i64::MAX),
        |tokens| i64::try_from(tokens).unwrap_or(i64::MAX),
    )
}

/// Compacts for replayed step-boundary pressure or one
/// provider-confirmed context overflow (dsh `compactIfNeeded`):
/// `Ok(None)` when no summary ran, the latest result otherwise.
///
/// # Errors
///
/// Returns the pipeline's failure face: the target config rejection,
/// the transaction's errors, or the retries-exhausted diagnostic.
pub fn compact_if_needed(
    view: &mut dyn SessionView,
    deps: &mut EngineDeps<'_>,
    config: &ResolvedConfig,
    trigger: CompactionTrigger,
) -> Result<Option<CompactionResult>, Box<EngineError>> {
    let Some((provider, model)) = routed_target(view) else {
        return Ok(None);
    };
    let policy = resolve_target_policy(config, &provider, &model);
    let mut measurement = measure(view, deps.meter)?;

    if trigger == CompactionTrigger::ContextOverflow {
        // Overflow always qualifies; the retained-tail policy bypasses
        // to force one useful balanced reduction.
        let Some((start, end)) = select(view, &measurement, 0)? else {
            return Ok(None);
        };
        return compact_region(view, deps, start, end)
            .map(Some)
            .map_err(|error| Box::new(EngineError::Region(*error)));
    }

    let info = (deps.model_capacity)(&provider, &model)
        .map_err(|error| Box::new(EngineError::Plain(error)))?;
    assert_no_active_compaction(view, "automatic pressure compaction")
        .map_err(|error| Box::new(EngineError::Region(CompactionError::Manual(error))))?;
    let target_key = format!("{provider}/{model}");
    let Some(context_window) = info.context_window else {
        return Err(Box::new(EngineError::TargetPressure(
            TargetPressureConfigError::new(
                target_key.clone(),
                format!(
                    "compaction-basic: no context capacity for {target_key}; \
                     configure contextWindow on that adapter model"
                ),
            ),
        )));
    };
    let spec = resolve_compact_spec(
        &policy,
        context_window,
        reserved_completion_tokens(view, info.default_max_tokens),
    )
    .map_err(|error| Box::new(EngineError::TargetPressure(error)))?;
    if below(&measurement, &spec) {
        return Ok(None);
    }

    // Once pressure qualifies, the model-free pass lands before choosing
    // a summary range (the optional pruner's face; absent here, the
    // measurement stands) and the retry loop re-selects against each
    // post-compaction measurement.
    let mut result: Option<CompactionResult> = None;
    let attempts = usize::try_from(spec.compaction_retries.max(0)).unwrap_or(usize::MAX);
    for _attempt in 0..=attempts {
        let Some((start, end)) = select(
            view,
            &measurement,
            spec.retain_tokens.max(0).cast_unsigned(),
        )?
        else {
            if result.is_none() {
                return Ok(None);
            }
            break;
        };
        result = Some(
            compact_region(view, deps, start, end)
                .map_err(|error| Box::new(EngineError::Region(*error)))?,
        );
        measurement = measure(view, deps.meter)?;
        if below(&measurement, &spec) {
            return Ok(result);
        }
    }

    Err(Box::new(EngineError::Plain(format!(
        "compaction still above threshold after {} compaction attempts \
         ({} estimated tokens >= threshold {})",
        attempts + 1,
        measurement.total_tokens,
        spec.threshold_tokens
    ))))
}

/// Prices the session through the injected meter (the dsh singleton
/// replay fold).
fn measure(
    view: &dyn SessionView,
    meter: &dyn crate::region::MeterView,
) -> Result<crate::meter::SurfaceMeasurement, Box<EngineError>> {
    meter
        .measure(view)
        .map_err(|error| Box::new(EngineError::Plain(error)))
}

/// Selects one compactable range (the balance-checked selection).
fn select(
    view: &dyn SessionView,
    measurement: &crate::meter::SurfaceMeasurement,
    retain_tokens: u64,
) -> Result<Option<(fr_wire::SessionSeq, fr_wire::SessionSeq)>, Box<EngineError>> {
    select_compactable_range(view, measurement, retain_tokens)
        .map_err(|error| Box::new(EngineError::Plain(error)))
}

/// The pressure gate: the measured total under the resolved threshold.
fn below(measurement: &crate::meter::SurfaceMeasurement, spec: &ResolvedCompactSpec) -> bool {
    measurement.total_tokens < u64::try_from(spec.threshold_tokens.max(0)).unwrap_or(u64::MAX)
}

/// Compacts one inclusive positional range with the auto-path options
/// (dsh `compactRegion`: `current-turn` owner, whole-surface stability,
/// no flush, no source command).
fn compact_region(
    view: &mut dyn SessionView,
    deps: &mut EngineDeps<'_>,
    start: fr_wire::SessionSeq,
    end: fr_wire::SessionSeq,
) -> Result<CompactionResult, Box<CompactionError>> {
    let mut region_deps = RegionDeps {
        meter: deps.meter,
        summarize: deps.summarize,
        recover: deps.recover,
    };
    let mut options = TransactionOptions {
        owner: crate::region::BracketOwner::CurrentTurn,
        stability: crate::region::StabilityRule::WholeSurface,
        flush: None,
        source_command_id: None,
    };
    compact_surface_region(&mut region_deps, view, start, end, &mut options).map_err(Box::new)
}

#[cfg(test)]
mod tests {
    use super::{
        EngineDeps, ModelCapacity, compact_if_needed, conversation_target,
        reserved_completion_tokens, routed_target,
    };
    use crate::config::{ResolvedRetention, resolve_config};
    use crate::meter::{MeterSurfaceNode, SurfaceMeasurement};
    use crate::region::MeterView;
    use crate::region::{AppendRequest, SessionView};
    use crate::summarizer::{CompactionFailure, SummarizationInput, SummaryResult};
    use crate::types::{CompactionResult, CompactionTrigger};
    use fr_wire::SessionSeq;
    use fr_wire::event::EpochHeader;
    use fr_wire::message::Message;
    use fr_wire::{ContentBlock, KnownContentBlock};

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Text {
            text: text.to_owned(),
        })
    }

    fn summary_of(text: &str) -> SummaryResult {
        SummaryResult {
            summary: vec![text_block(text)],
            raw_output: Some(vec![text_block(text)]),
            llm_stream_call: true,
            provider: "mock".to_owned(),
            model: "mock".to_owned(),
            max_tokens: Some(4_096),
            usage: None,
        }
    }

    fn user_message(text: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "role": "user",
            "id": format!("msg-{text}"),
            "content": [{ "type": "text", "text": text }],
            "source": { "kind": "user" },
        }))
        .expect("fixture message")
    }

    /// The in-memory session double over the same fold the region
    /// transaction drives (the region.rs test fixture shape).
    struct FakeSession {
        log: Vec<fr_wire::SessionEvent>,
        surface: Vec<SessionSeq>,
        generation: u64,
        header: Option<EpochHeader>,
    }

    impl FakeSession {
        fn new() -> Self {
            Self {
                log: Vec::new(),
                surface: Vec::new(),
                generation: 0,
                header: None,
            }
        }

        fn set_header(&mut self, provider: &str, model: &str, max_tokens: u64) {
            let header: EpochHeader = serde_json::from_value(serde_json::json!({
                "config": {
                    "provider": provider,
                    "model": model,
                    "maxTokens": max_tokens,
                },
            }))
            .expect("fixture header");
            self.header = Some(header);
        }

        fn seed_open_turn(&mut self) {
            let _ = self
                .append(AppendRequest {
                    type_name: "turn/start",
                    data: serde_json::json!({ "turn": 1 }),
                    surface_op: None,
                    source_event_seqs: None,
                })
                .expect("seed turn");
        }

        fn seed_messages(&mut self, count: usize) {
            for index in 0..count {
                let message = user_message(&format!("turn {index}"));
                let data = serde_json::json!({ "message": message });
                let _ = self
                    .append(AppendRequest {
                        type_name: "user/message",
                        data,
                        surface_op: None,
                        source_event_seqs: None,
                    })
                    .expect("seed append");
            }
        }
    }

    impl SessionView for FakeSession {
        fn events(&self) -> &[fr_wire::SessionEvent] {
            &self.log
        }

        fn surface_nodes(&self) -> &[SessionSeq] {
            &self.surface
        }

        fn replace_generation(&self) -> u64 {
            self.generation
        }

        fn request_header(&self) -> Option<&EpochHeader> {
            self.header.as_ref()
        }

        fn append(&mut self, request: AppendRequest<'_>) -> Result<fr_wire::SessionEvent, String> {
            let event: fr_wire::SessionEvent = serde_json::from_value(serde_json::json!({
                "type": request.type_name,
                "seq": self.log.len() as u64,
                "time": 1_700_000_000_000_i64,
                "data": request.data,
            }))
            .map_err(|error| format!("fixture event decode: {error}"))?;
            if matches!(
                request.type_name,
                "user/message" | "assistant/message" | "system/message"
            ) {
                self.surface.push(event.seq);
            }
            self.log.push(event);
            Ok(self.log.last().expect("just pushed").clone())
        }
    }

    /// The fixed-price meter double (the region.rs fixture shape).
    struct FakeMeter {
        per_node: u64,
        checkpoint_estimate: u64,
    }

    impl FakeMeter {
        fn new(per_node: u64) -> Self {
            Self {
                per_node,
                checkpoint_estimate: 20,
            }
        }
    }

    impl MeterView for FakeMeter {
        fn measure(&self, session: &dyn SessionView) -> Result<SurfaceMeasurement, String> {
            let nodes: Vec<MeterSurfaceNode> = session
                .surface_nodes()
                .iter()
                .map(|seq| MeterSurfaceNode {
                    seq: seq.get(),
                    tokens: self.per_node,
                    heuristic_tokens: self.per_node,
                })
                .collect();
            let total = self.per_node * nodes.len() as u64;
            Ok(SurfaceMeasurement {
                log_revision: session.events().len() as u64,
                baseline: crate::meter::MeasurementBaseline::None { tokens: 0 },
                surface_delta_tokens: i64::try_from(total).unwrap_or(i64::MAX),
                total_tokens: total,
                surface_tokens: total,
                nodes,
            })
        }

        fn estimate_message(&self, _message: &Message) -> u64 {
            self.checkpoint_estimate
        }
    }

    fn deps<'a>(
        meter: &'a FakeMeter,
        summarize: &'a mut dyn FnMut(
            &SummarizationInput,
        ) -> Result<SummaryResult, CompactionFailure>,
        recover: &'a mut dyn FnMut(&CompactionFailure, &[SessionSeq]) -> bool,
        capacity: &'a mut dyn FnMut(&str, &str) -> Result<ModelCapacity, String>,
    ) -> EngineDeps<'a> {
        EngineDeps {
            meter,
            summarize,
            recover,
            model_capacity: capacity,
        }
    }

    fn config() -> crate::config::ResolvedConfig {
        resolve_config(&serde_json::json!({})).expect("default config resolves")
    }

    fn no_capacity(_provider: &str, _model: &str) -> ModelCapacity {
        ModelCapacity {
            context_window: None,
            default_max_tokens: None,
        }
    }

    fn capacity(window: i64) -> impl Fn(&str, &str) -> Result<ModelCapacity, String> {
        move |_provider, _model| {
            Ok(ModelCapacity {
                context_window: Some(window),
                default_max_tokens: Some(4_096),
            })
        }
    }

    fn ok_summarize(_input: &SummarizationInput) -> SummaryResult {
        summary_of("a summary that frames small")
    }

    fn no_recover() -> impl FnMut(&CompactionFailure, &[SessionSeq]) -> bool {
        |_failure, _seqs| false
    }

    #[test]
    fn routed_and_conversation_targets_walk_the_sources() {
        let mut session = FakeSession::new();
        assert_eq!(routed_target(&session), None);
        assert_eq!(
            conversation_target(&session, Some(("agent-p", "agent-m"))),
            Some(("agent-p".to_owned(), "agent-m".to_owned()))
        );
        session.set_header("deepseek", "m1", 8_192);
        assert_eq!(
            routed_target(&session),
            Some(("deepseek".to_owned(), "m1".to_owned()))
        );
        assert_eq!(
            conversation_target(&session, Some(("", ""))),
            Some(("deepseek".to_owned(), "m1".to_owned())),
            "an empty agent route never overrides the routed pair"
        );
    }

    #[test]
    fn reserved_tokens_prefer_the_envelope_cap_then_the_adapter_default() {
        let mut session = FakeSession::new();
        session.set_header("deepseek", "m1", 1_024);
        assert_eq!(reserved_completion_tokens(&session, Some(8_192)), 1_024);
        let bare = FakeSession::new();
        assert_eq!(reserved_completion_tokens(&bare, Some(8_192)), 8_192);
        assert_eq!(reserved_completion_tokens(&bare, None), 0);
    }

    #[test]
    fn unrouted_sessions_never_compact() {
        let mut session = FakeSession::new();
        session.seed_messages(5);
        let meter = FakeMeter::new(1_000);
        let summarize =
            &mut |input: &SummarizationInput| Ok::<_, CompactionFailure>(ok_summarize(input));
        let mut recover = no_recover();
        let mut capacity = capacity(262_144);
        let mut engine = deps(&meter, summarize, &mut recover, &mut capacity);
        let outcome = compact_if_needed(
            &mut session,
            &mut engine,
            &config(),
            CompactionTrigger::Pressure,
        )
        .expect("unrouted sessions return null");
        assert_eq!(outcome, None);
    }

    #[test]
    fn pressure_below_threshold_never_compacts() {
        let mut session = FakeSession::new();
        session.set_header("deepseek", "m1", 1_024);
        session.seed_messages(2);
        let meter = FakeMeter::new(10);
        let summarize =
            &mut |input: &SummarizationInput| Ok::<_, CompactionFailure>(ok_summarize(input));
        let mut recover = no_recover();
        let mut capacity = capacity(262_144);
        let mut engine = deps(&meter, summarize, &mut recover, &mut capacity);
        let outcome = compact_if_needed(
            &mut session,
            &mut engine,
            &config(),
            CompactionTrigger::Pressure,
        )
        .expect("under-threshold sessions return null");
        assert_eq!(outcome, None);
    }

    #[test]
    fn missing_capacity_rejects_with_the_target_key() {
        let mut session = FakeSession::new();
        session.set_header("deepseek", "m1", 1_024);
        session.seed_messages(4);
        let meter = FakeMeter::new(1_000);
        let summarize =
            &mut |input: &SummarizationInput| Ok::<_, CompactionFailure>(ok_summarize(input));
        let mut recover = no_recover();
        let cap = &mut |provider: &str, model: &str| Ok::<_, String>(no_capacity(provider, model));
        let mut engine = deps(&meter, summarize, &mut recover, cap);
        let error = compact_if_needed(
            &mut session,
            &mut engine,
            &config(),
            CompactionTrigger::Pressure,
        )
        .expect_err("no capacity");
        assert_eq!(
            error.target_key(),
            Some("deepseek/m1"),
            "the warned-once suppression key rides the failure"
        );
        assert_eq!(
            error.message(),
            "compaction-basic: no context capacity for deepseek/m1; configure \
             contextWindow on that adapter model"
        );
    }

    #[test]
    fn overflow_compacts_once_bypassing_the_threshold() {
        let mut session = FakeSession::new();
        session.set_header("deepseek", "m1", 1_024);
        session.seed_open_turn();
        session.seed_messages(4);
        let meter = FakeMeter::new(10);
        let summarize =
            &mut |input: &SummarizationInput| Ok::<_, CompactionFailure>(ok_summarize(input));
        let mut recover = no_recover();
        let mut cap = capacity(262_144);
        let mut engine = deps(&meter, summarize, &mut recover, &mut cap);
        let outcome = compact_if_needed(
            &mut session,
            &mut engine,
            &config(),
            CompactionTrigger::ContextOverflow,
        )
        .expect("overflow compaction lands");
        let result: CompactionResult = outcome.expect("one compaction ran");
        let types: Vec<&str> = session
            .events()
            .iter()
            .map(fr_wire::SessionEvent::event_type)
            .collect();
        assert!(types.contains(&"compaction/start"));
        assert!(types.contains(&"compaction/summary"));
        assert!(types.contains(&"compaction/end"));
        assert!(result.start_seq.get() > 0);
    }

    #[test]
    fn pressure_retry_loop_exhausts_with_the_verbatim_error() {
        let mut session = FakeSession::new();
        session.set_header("deepseek", "m1", 1_024);
        session.seed_open_turn();
        session.seed_messages(4);
        let meter = FakeMeter::new(60_000);
        let summarize =
            &mut |input: &SummarizationInput| Ok::<_, CompactionFailure>(ok_summarize(input));
        let mut recover = no_recover();
        let mut cap = capacity(262_144);
        let mut engine = deps(&meter, summarize, &mut recover, &mut cap);
        let error = compact_if_needed(
            &mut session,
            &mut engine,
            &config(),
            CompactionTrigger::Pressure,
        )
        .expect_err("the fixed-price meter never drops below the threshold");
        let default_config = config();
        let attempts = default_config.compaction_retries + 1;
        let policy = crate::config::resolve_target_policy(&default_config, "deepseek", "m1");
        let spec = crate::config::resolve_compact_spec(&policy, 262_144, 1_024)
            .expect("fixture spec resolves");
        let final_total = session.surface_nodes().len() as u64 * 60_000;
        assert_eq!(
            error.message(),
            format!(
                "compaction still above threshold after {attempts} compaction attempts \
                 ({final_total} estimated tokens >= threshold {})",
                spec.threshold_tokens
            ),
            "the dsh retries-exhausted text pins verbatim"
        );
    }

    #[test]
    fn retention_policy_reaches_the_selection() {
        let resolved = resolve_config(&serde_json::json!({
            "retain-tokens": 50,
        }))
        .expect("configured retention resolves");
        assert_eq!(resolved.retention, ResolvedRetention::Tokens(50));
    }
}
