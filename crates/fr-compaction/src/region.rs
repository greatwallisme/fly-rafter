//! Surface retention selection and the shared log-recorded compaction
//! transaction (dsh compaction-basic/src/region.ts 1:1).
//!
//! The dsh module runs against a live in-process `Session`; fr runs against
//! the [`SessionView`] seam the engine shell implements (the session
//! snapshot plus the append service face, Q107-A). The summarizer hook and
//! the summary-error recovery hook ride [`RegionDeps`]; the recovery hook
//! forwards to the kernel `compaction/summary-error` waterfall with the
//! identity `false` fallback at 17f.
//!
//! Documented structurals: dsh checks the abort signal at four points of
//! the transaction (`throwIfAborted`); fr has no abort carrier at this
//! layer — the only yield point inside the transaction is the summarize
//! hook, whose own cancellation rides the 17f wiring, so those checks land
//! there.

use crate::meter::{MeterSurfaceNode, SurfaceMeasurement};
use fr_session::derive_event_message;
use fr_wire::event_type;
use fr_wire::message::{KnownMessage, MessageSource};
use fr_wire::{
    ContentBlock, EpochHeader, Message, MessageId, RawObject, ReplaceOp, ReplaceTag, SessionEvent,
    SessionSeq, SurfaceOp,
};
use serde::Serialize;
use uuid::Uuid;

use crate::brand::CompactionId;
use crate::checkpoint::compact_checkpoint_source;
use crate::summarizer::{CompactionFailure, SummarizationInput, SummaryResult, frame_summary};
use crate::tool_pairing::{
    balance_cache, tool_pairing_balanced_after, tool_pairing_balanced_before,
};
use crate::types::{
    CompactionEndData, CompactionResult, CompactionStartData, CompactionSummaryData,
    ManualCompactionError, ManualCompactionErrorCode, ShadowedRange,
};

/// The session read-and-append face the compaction transaction consumes
/// (dsh live `Session`; the Q107-A read face over the wire).
///
/// The implementing shell keeps the view consistent with its own appends:
/// every append reflects in [`events`](Self::events),
/// [`surface_nodes`](Self::surface_nodes), and
/// [`replace_generation`](Self::replace_generation) for subsequent reads
/// inside the same transaction.
pub trait SessionView {
    /// The durable log, in ascending seq order.
    fn events(&self) -> &[SessionEvent];

    /// The current surface node seqs, in surface order.
    fn surface_nodes(&self) -> &[SessionSeq];

    /// The surface rewrite generation (bumped by every replacement).
    fn replace_generation(&self) -> u64;

    /// The latest durable `request/header` snapshot, when one exists.
    fn request_header(&self) -> Option<&EpochHeader>;

    /// The projected message for one event seq, when a committed
    /// message projection replaced it (the dsh `projectedMessages`
    /// lookup; the derive faces consult it before the raw event).
    fn projected_message(&self, seq: SessionSeq) -> Option<&fr_wire::message::Message> {
        let _ = seq;
        None
    }

    /// Append one event and reflect it in the view.
    ///
    /// # Errors
    ///
    /// Returns the session admission diagnostic verbatim when the store
    /// refuses the event.
    fn append(&mut self, request: AppendRequest<'_>) -> Result<SessionEvent, String>;
}

/// The summary-error recovery hook's seam: `true` re-prepares and
/// retries; the view rides the call so a durable recovery reflects in
/// the transaction's own view.
pub type RecoverHook<'a> =
    &'a mut dyn FnMut(&CompactionFailure, &[SessionSeq], &mut dyn SessionView) -> bool;

/// One decoded append: type key, raw data object, and the optional surface
/// metadata (the session-service append wire face).
#[derive(Debug, Clone)]
pub struct AppendRequest<'a> {
    /// The wire event type name.
    pub type_name: &'a str,
    /// The data object exactly as stored.
    pub data: serde_json::Value,
    /// How the event joins the surface, when it produces a message.
    pub surface_op: Option<SurfaceOp>,
    /// Complete set of earlier source-event seqs this event cites.
    pub source_event_seqs: Option<Vec<SessionSeq>>,
}

/// The conversation meter face the transaction consumes (dsh `TokenMeter`
/// reads through `ctx.tokenMeter`).
pub trait MeterView {
    /// Measure the session through the replay fold.
    ///
    /// # Errors
    ///
    /// Returns the meter diagnostic verbatim.
    fn measure(&self, session: &dyn SessionView) -> Result<SurfaceMeasurement, String>;

    /// Price one message under the fixed heuristic (the checkpoint shrink
    /// gate's unit).
    fn estimate_message(&self, message: &Message) -> u64;
}

/// The conversation meter plus the dynamically dispatched summarizer hook
/// (dsh `RegionDependencies`).
pub struct RegionDeps<'a> {
    /// The effective token meter.
    pub meter: &'a dyn MeterView,
    /// The summarizer hook — the engine's sole customization point.
    pub summarize:
        &'a mut dyn FnMut(&SummarizationInput) -> Result<SummaryResult, CompactionFailure>,
    /// The summary-error recovery hook; returns whether the failure was
    /// durably recovered and the summarizer should re-prepare and
    /// retry. The view rides the call so a durable recovery (the
    /// image-offload decision) reflects in the transaction's own view
    /// before the re-prepare.
    pub recover: RecoverHook<'a>,
}

impl std::fmt::Debug for RegionDeps<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegionDeps").finish_non_exhaustive()
    }
}

/// One validated inclusive span of current surface positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceSelection {
    /// Inclusive first surface-node seq.
    pub start: SessionSeq,
    /// Inclusive last surface-node seq.
    pub end: SessionSeq,
    /// Position of `start` on the current surface.
    pub start_idx: usize,
    /// Position of `end` on the current surface.
    pub end_idx: usize,
    /// The shadowed surface nodes, in surface order.
    pub shadowed_seqs: Vec<SessionSeq>,
}

/// A selection with its priced snapshot and the replay input built from it.
#[derive(Debug, Clone)]
pub struct PreparedCompaction {
    /// The validated span.
    pub selection: SurfaceSelection,
    /// The priced snapshot the preparation measured.
    pub measurement: SurfaceMeasurement,
    /// The selected nodes, in surface order.
    pub selected_nodes: Vec<MeterSurfaceNode>,
    /// Heuristic total of the selected span (the shadow-price protocol's
    /// replacement price).
    pub shadowed_token_count: u64,
    /// Route-priced total of the selected span; the shrink comparison's
    /// unit.
    pub shadowed_route_token_count: u64,
    /// The replay input built from the selection.
    pub input: SummarizationInput,
}

/// A summarization result paired with its framed checkpoint message.
#[derive(Debug, Clone)]
pub struct SummarizedCompaction {
    /// The prepared selection the summary was built from.
    pub prepared: PreparedCompaction,
    /// The summarizer's result.
    pub result: SummaryResult,
    /// The framed replacement user message.
    pub checkpoint_message: Message,
}

/// Whose bracket the transaction writes (dsh `options.owner`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketOwner {
    /// Derive a numbered owner from the open turn.
    CurrentTurn,
    /// Write a standalone bracket between turns.
    Standalone,
}

/// The surface relationship that must survive summarization (dsh
/// `options.stability`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StabilityRule {
    /// Reject a summary prepared against any earlier surface generation.
    WholeSurface,
    /// Require only that the selected span remain the same present,
    /// contiguous, equally priced, balanced replacement target.
    SelectedSpan,
}

/// The transaction options (dsh `CompactionTransactionOptions`).
pub struct TransactionOptions<'a> {
    /// Whose bracket to write.
    pub owner: BracketOwner,
    /// The stability rule.
    pub stability: StabilityRule,
    /// Optional durability checkpoint after a successfully closed bracket.
    pub flush: Option<&'a mut dyn FnMut() -> Result<(), String>>,
    /// Manual command that initiated this transaction, when present.
    pub source_command_id: Option<String>,
}

impl std::fmt::Debug for TransactionOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionOptions")
            .field("owner", &self.owner)
            .field("stability", &self.stability)
            .field("flush", &self.flush.as_ref().map(|_| ()))
            .field("source_command_id", &self.source_command_id)
            .finish()
    }
}

/// The open-turn, unmatched-compaction, and seed-boundary state at
/// transaction entry (dsh `CompactionEntryState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionEntryState {
    /// The open turn ordinal, when the log ends inside a turn.
    pub open_turn: Option<u64>,
    /// The latest unmatched `compaction/start` seq, when the log ends
    /// inside a compaction bracket.
    pub unmatched_start_seq: Option<SessionSeq>,
    /// The newest constructor-seed boundary seq, when one exists.
    pub latest_end_seed_seq: Option<SessionSeq>,
}

/// The failure face of the compaction transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionError {
    /// The classified manual-compaction failure: entry `busy` rejections,
    /// the in-transaction manual classification, and the persistence face.
    Manual(ManualCompactionError),
    /// A failure carrying its in-transaction classification — verbatim on
    /// the automatic path, classified into
    /// [`CompactionError::Manual`] on the manual path.
    Failure(CompactionFailure),
}

/// The bracket identity carried through every lifecycle event (dsh
/// `lifecycle`).
struct BracketLifecycle<'a> {
    compaction_id: &'a CompactionId,
    source_command_id: Option<&'a str>,
    turn: Option<u64>,
}

impl std::fmt::Debug for BracketLifecycle<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BracketLifecycle")
            .field("compaction_id", &self.compaction_id.0)
            .field("source_command_id", &self.source_command_id)
            .field("turn", &self.turn)
            .finish()
    }
}

/// The in-transaction stage a failure happened at (dsh
/// `TransactionFailure.stage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Summary,
    Commit,
}

/// The committed body of one compaction, awaiting its close event (dsh
/// `Omit<CompactionResult, 'endSeq'>`).
struct PendingResult {
    compaction_id: CompactionId,
    source_command_id: Option<String>,
    start_seq: SessionSeq,
    summary_seq: SessionSeq,
    end_seq: SessionSeq,
    summary: Vec<ContentBlock>,
    shadowed_range: ShadowedRange,
    shadowed_seqs: Vec<SessionSeq>,
    shadowed_token_count: u64,
}

/// Mint one implementation-compaction identity (dsh
/// `CompactionId(randomUUID())`).
#[must_use]
pub fn mint_compaction_id() -> CompactionId {
    CompactionId(Uuid::new_v4().to_string())
}

/// Find one event by seq in the seq-ordered durable log.
pub(crate) fn event_at(events: &[SessionEvent], seq: SessionSeq) -> Option<&SessionEvent> {
    let index = events.binary_search_by_key(&seq, |event| event.seq).ok()?;
    Some(&events[index])
}

/// The `system/message` holding surface node 0, or `None` when another
/// message-producing event starts the surface.
fn system_head(events: &[SessionEvent], head_seq: SessionSeq) -> Option<&SessionEvent> {
    let head = event_at(events, head_seq)?;
    (head.event_type() == event_type::SYSTEM_MESSAGE).then_some(head)
}

/// Resolve the next range starting at the first non-system surface node
/// while retaining a priced recent tail and never splitting an assistant
/// tool-call/result pair (dsh `selectCompactableRange`).
///
/// A `system/message` at surface node 0 is never inside the range; without
/// one the range starts at node 0.
///
/// # Errors
///
/// Returns the dsh verbatim failure when the priced surface does not match
/// the current session surface, or when the tool-pairing fold rejects the
/// log.
pub fn select_compactable_range(
    session: &dyn SessionView,
    measurement: &SurfaceMeasurement,
    retain_tokens: u64,
) -> Result<Option<(SessionSeq, SessionSeq)>, String> {
    let priced_nodes = &measurement.nodes;
    if priced_nodes.is_empty() {
        return Ok(None);
    }
    let surface_nodes = session.surface_nodes();
    if surface_nodes.len() != priced_nodes.len()
        || surface_nodes
            .iter()
            .zip(priced_nodes.iter())
            .any(|(seq, node)| seq.get() != node.seq)
    {
        return Err(
            "compaction: token-meter surface does not match the current session surface".to_owned(),
        );
    }
    let first_idx = usize::from(system_head(session.events(), surface_nodes[0]).is_some());

    let mut accumulated = 0u64;
    let mut keep_from_idx = priced_nodes.len();
    for (index, node) in priced_nodes.iter().enumerate().rev() {
        accumulated += node.tokens;
        keep_from_idx = index;
        if accumulated >= retain_tokens {
            break;
        }
    }
    if keep_from_idx <= first_idx {
        return Ok(None);
    }

    let balance = balance_cache(
        None,
        session.replace_generation(),
        surface_nodes,
        session.events(),
    )?;
    while keep_from_idx > first_idx {
        if tool_pairing_balanced_before(&balance, surface_nodes[keep_from_idx])? {
            break;
        }
        keep_from_idx -= 1;
    }
    if keep_from_idx <= first_idx {
        return Ok(None);
    }

    Ok(Some((
        surface_nodes[first_idx],
        surface_nodes[keep_from_idx - 1],
    )))
}

/// Validate one requested surface-position span before the transaction
/// begins (dsh `validateSurfaceRegion`).
///
/// # Errors
///
/// Returns the dsh verbatim failure for missing or reversed boundaries and
/// unbalanced cuts.
pub fn validate_surface_region(
    session: &dyn SessionView,
    start: SessionSeq,
    end: SessionSeq,
) -> Result<SurfaceSelection, String> {
    let nodes = session.surface_nodes();
    let start_idx = nodes.iter().position(|seq| *seq == start);
    let end_idx = nodes.iter().position(|seq| *seq == end);
    let Some(start_idx) = start_idx else {
        return Err(format!(
            "compactRegion: start seq {} not found in surface",
            start.get()
        ));
    };
    let Some(end_idx) = end_idx else {
        return Err(format!(
            "compactRegion: end seq {} not found in surface",
            end.get()
        ));
    };
    if start_idx > end_idx {
        return Err(format!(
            "compactRegion: start seq {} (position {start_idx}) is after end seq {} \
             (position {end_idx}) on the surface",
            start.get(),
            end.get()
        ));
    }
    let balance = balance_cache(None, session.replace_generation(), nodes, session.events())?;
    if !tool_pairing_balanced_before(&balance, nodes[start_idx])? {
        return Err(format!(
            "compactRegion: start seq {} is not a balanced boundary (would split a \
             step's tool-call/result pair)",
            start.get()
        ));
    }
    if !tool_pairing_balanced_after(&balance, nodes[end_idx])? {
        return Err(format!(
            "compactRegion: end seq {} is not a balanced boundary (would split a \
             step, or the step is still open)",
            end.get()
        ));
    }
    Ok(SurfaceSelection {
        start,
        end,
        start_idx,
        end_idx,
        shadowed_seqs: nodes[start_idx..=end_idx].to_vec(),
    })
}

/// Recheck the durable compaction lock after an asynchronous policy
/// decision (dsh `assertNoActiveCompaction`).
///
/// # Errors
///
/// Returns the busy classification when an unmatched marker is still
/// active.
pub fn assert_no_active_compaction(
    session: &dyn SessionView,
    stage: &str,
) -> Result<(), ManualCompactionError> {
    let entry = inspect_compaction_entry_state(session.events());
    assert_compaction_inactive(&entry, stage)
}

/// Reject a durable unmatched compaction marker unless a later
/// constructor-seed boundary proves that its owner belongs to an earlier
/// session lifecycle (dsh `assertCompactionInactive`).
fn assert_compaction_inactive(
    entry: &CompactionEntryState,
    stage: &str,
) -> Result<(), ManualCompactionError> {
    let stale_owner = entry
        .latest_end_seed_seq
        .is_some_and(|seed| entry.unmatched_start_seq.is_some_and(|start| seed > start));
    if entry.unmatched_start_seq.is_none() || stale_owner {
        return Ok(());
    }
    Err(ManualCompactionError::new(
        ManualCompactionErrorCode::Busy,
        format!(
            "{stage}: compaction already in progress; the session compaction lock is \
             already active"
        ),
    ))
}

/// Inspect open-turn, unmatched-compaction, and latest seed-boundary state
/// independently (dsh `inspectCompactionEntryState`).
#[must_use]
pub fn inspect_compaction_entry_state(events: &[SessionEvent]) -> CompactionEntryState {
    let mut state = CompactionEntryState {
        open_turn: None,
        unmatched_start_seq: None,
        latest_end_seed_seq: None,
    };
    let mut open_turn_known = false;
    let mut compaction_known = false;
    for event in events.iter().rev() {
        if state.latest_end_seed_seq.is_none() && event.event_type() == event_type::SESSION_END_SEED
        {
            state.latest_end_seed_seq = Some(event.seq);
        }
        if !compaction_known {
            if event.event_type() == crate::types::event_type::START {
                state.unmatched_start_seq = Some(event.seq);
                compaction_known = true;
            } else if event.event_type() == crate::types::event_type::END {
                compaction_known = true;
            }
        }
        if !open_turn_known {
            match &event.data {
                fr_wire::SessionEventData::TurnStart(data) => {
                    state.open_turn = Some(data.turn);
                    open_turn_known = true;
                }
                fr_wire::SessionEventData::TurnEnd(_) => open_turn_known = true,
                _ => {}
            }
        }
        if open_turn_known && compaction_known && state.latest_end_seed_seq.is_some() {
            break;
        }
    }
    state
}

/// Run the single compaction transaction over one selected positional span
/// (dsh `compactSurfaceRegion`).
///
/// Selection and validation are read-only. Idle/log validation and
/// `compaction/start` are synchronously adjacent, so the durable opening
/// marker is the compaction lock before summarization runs. Every later
/// failure makes exactly one `compaction/end` attempt; a failed close
/// deliberately leaves the unmatched start detectable.
///
/// # Errors
///
/// Returns [`CompactionError::Manual`] for the entry `busy` faces and the
/// classified manual path, and [`CompactionError::Failure`] for the entry
/// validation and the automatic in-transaction failures (rethrown
/// verbatim).
pub fn compact_surface_region(
    deps: &mut RegionDeps<'_>,
    session: &mut dyn SessionView,
    start: SessionSeq,
    end: SessionSeq,
    options: &mut TransactionOptions<'_>,
) -> Result<CompactionResult, CompactionError> {
    let selection = validate_surface_region(session, start, end)
        .map_err(|message| CompactionError::Failure(CompactionFailure::Other(message)))?;
    let entry = inspect_compaction_entry_state(session.events());
    assert_compaction_inactive(&entry, "compaction").map_err(CompactionError::Manual)?;

    let owner = resolve_owner(options.owner, entry.open_turn)?;

    let compaction_id = mint_compaction_id();
    let start_data = CompactionStartData {
        compaction_id: compaction_id.clone(),
        source_command_id: options.source_command_id.clone(),
        turn: owner,
    };
    let start_event = append_typed(
        session,
        crate::types::event_type::START,
        encode(&start_data)
            .map_err(|message| CompactionError::Failure(CompactionFailure::Other(message)))?,
        None,
        None,
    )
    .map_err(|message| CompactionError::Failure(CompactionFailure::Other(message)))?;

    let mut closed = false;
    let mut pending: Option<PendingResult> = None;
    let mut failure: Option<(CompactionFailure, Stage)> = None;
    let lifecycle = BracketLifecycle {
        compaction_id: &compaction_id,
        source_command_id: options.source_command_id.as_deref(),
        turn: owner,
    };
    match run_body(
        deps,
        session,
        &selection,
        &lifecycle,
        start_event.seq,
        options.stability,
    ) {
        Ok(body) => {
            closed = true;
            pending = Some(body);
        }
        Err((error, stage, close_attempted)) => {
            if close_attempted {
                failure = Some((error, stage));
            } else {
                // Exactly one close attempt: a failed close deliberately
                // leaves the unmatched start detectable.
                match close_bracket_with_failure(
                    session,
                    &compaction_id,
                    options.source_command_id.as_deref(),
                    owner,
                    &error,
                ) {
                    Ok(()) => {
                        closed = true;
                        failure = Some((error, stage));
                    }
                    Err(close_error) => {
                        failure = Some((CompactionFailure::Other(close_error), Stage::Commit));
                    }
                }
            }
        }
    }

    let flush_failure = closed && options.flush.as_mut().is_some_and(|flush| flush().is_err());

    if let Some((error, stage)) = failure {
        return match options.owner {
            BracketOwner::CurrentTurn => Err(CompactionError::Failure(error)),
            BracketOwner::Standalone => {
                Err(CompactionError::Manual(throw_manual_failure(&error, stage)))
            }
        };
    }
    if flush_failure {
        return Err(CompactionError::Manual(ManualCompactionError::new(
            ManualCompactionErrorCode::Persistence,
            "manual compaction durability checkpoint failed",
        )));
    }
    let body = pending.ok_or_else(|| {
        CompactionError::Failure(CompactionFailure::Other(
            "compaction committed without a result".to_owned(),
        ))
    })?;
    Ok(CompactionResult {
        compaction_id: body.compaction_id,
        source_command_id: body.source_command_id,
        start_seq: body.start_seq,
        summary_seq: body.summary_seq,
        end_seq: body.end_seq,
        summary: body.summary,
        shadowed_range: body.shadowed_range,
        shadowed_seqs: body.shadowed_seqs,
        shadowed_token_count: body.shadowed_token_count,
    })
}

/// Resolve the bracket owner from the requested face and the open turn
/// (dsh's owner branch).
fn resolve_owner(
    requested: BracketOwner,
    open_turn: Option<u64>,
) -> Result<Option<u64>, CompactionError> {
    match requested {
        BracketOwner::Standalone => {
            if open_turn.is_some() {
                return Err(CompactionError::Manual(ManualCompactionError::new(
                    ManualCompactionErrorCode::Busy,
                    "manual compaction: the session already has an open turn",
                )));
            }
            Ok(None)
        }
        BracketOwner::CurrentTurn => open_turn.map_or_else(
            || {
                Err(CompactionError::Failure(CompactionFailure::Other(
                    "compactRegion: no open turn — automatic compaction events must \
                     be enclosed in a turn"
                        .to_owned(),
                )))
            },
            |turn| Ok(Some(turn)),
        ),
    }
}

/// Append the error-bearing close event (dsh's catch-block close attempt).
fn close_bracket_with_failure(
    session: &mut dyn SessionView,
    compaction_id: &CompactionId,
    source_command_id: Option<&str>,
    turn: Option<u64>,
    error: &CompactionFailure,
) -> Result<(), String> {
    let end_data = CompactionEndData {
        compaction_id: compaction_id.clone(),
        source_command_id: source_command_id.map(str::to_owned),
        turn,
        error: Some(error.message().to_owned()),
    };
    append_typed(
        session,
        crate::types::event_type::END,
        encode(&end_data)?,
        None,
        None,
    )
    .map(|_| ())
}

/// Run the prepare → summarize → stability → commit body (dsh's try block).
///
/// Returns `(failure, stage, close_attempted)`; `close_attempted` marks the
/// failures that happened after the body's own `compaction/end` attempt, so
/// the caller does not close twice.
fn run_body(
    deps: &mut RegionDeps<'_>,
    session: &mut dyn SessionView,
    selection: &SurfaceSelection,
    lifecycle: &BracketLifecycle<'_>,
    start_seq: SessionSeq,
    rule: StabilityRule,
) -> Result<PendingResult, (CompactionFailure, Stage, bool)> {
    let prepared = prepare_compaction(deps.meter, session, selection)
        .map_err(|failure| (failure, Stage::Summary, false))?;
    let summarized = summarize_compaction(
        deps,
        session,
        prepared,
        lifecycle.compaction_id,
        lifecycle.source_command_id,
        rule,
    )
    .map_err(|(failure, stage)| (failure, stage, false))?;
    assert_stable(rule, deps.meter, session, &summarized.prepared)
        .map_err(|failure| (failure, Stage::Summary, false))?;
    let mut pending = commit_compaction_body(
        session,
        start_seq,
        lifecycle.compaction_id,
        lifecycle.source_command_id,
        &summarized,
    )
    .map_err(|message| (CompactionFailure::Other(message), Stage::Commit, false))?;
    let end_data = CompactionEndData {
        compaction_id: lifecycle.compaction_id.clone(),
        source_command_id: lifecycle.source_command_id.map(str::to_owned),
        turn: lifecycle.turn,
        error: None,
    };
    let end_event = append_typed(
        session,
        crate::types::event_type::END,
        encode(&end_data)
            .map_err(|message| (CompactionFailure::Other(message), Stage::Commit, true))?,
        None,
        None,
    )
    .map_err(|message| (CompactionFailure::Other(message), Stage::Commit, true))?;
    pending.end_seq = end_event.seq;
    Ok(pending)
}

/// Classify one closed manual attempt without weakening cancellation
/// precedence (dsh `throwManualFailure`).
fn throw_manual_failure(failure: &CompactionFailure, stage: Stage) -> ManualCompactionError {
    if stage == Stage::Commit {
        return ManualCompactionError::new(
            ManualCompactionErrorCode::Commit,
            "manual compaction did not commit cleanly",
        );
    }
    match failure {
        CompactionFailure::SurfaceChanged(_) => ManualCompactionError::new(
            ManualCompactionErrorCode::Changed,
            "the compacted history changed during manual compaction",
        ),
        _ => ManualCompactionError::new(
            ManualCompactionErrorCode::Summary,
            "manual compaction could not produce a smaller summary",
        ),
    }
}

/// Snapshot pricing and replay input for a validated surface range (dsh
/// `prepareCompaction`).
fn prepare_compaction(
    meter: &dyn MeterView,
    session: &dyn SessionView,
    selection: &SurfaceSelection,
) -> Result<PreparedCompaction, CompactionFailure> {
    let measurement = meter.measure(session).map_err(CompactionFailure::Other)?;
    let selected_nodes = measurement.nodes[selection.start_idx..=selection.end_idx].to_vec();
    if selected_nodes.len() != selection.shadowed_seqs.len()
        || selected_nodes
            .iter()
            .zip(selection.shadowed_seqs.iter())
            .any(|(node, seq)| node.seq != seq.get())
    {
        return Err(CompactionFailure::SurfaceChanged(
            "compaction: selected surface changed before summarization began".to_owned(),
        ));
    }
    let shadowed_token_count = selected_nodes
        .iter()
        .map(|node| node.heuristic_tokens)
        .sum();
    let shadowed_route_token_count = selected_nodes.iter().map(|node| node.tokens).sum();
    let input = build_summarization_input(session, &selection.shadowed_seqs);
    Ok(PreparedCompaction {
        selection: selection.clone(),
        measurement,
        selected_nodes,
        shadowed_token_count,
        shadowed_route_token_count,
        input,
    })
}

/// Run the summarizer, its recovery loop, and the shrink gate (dsh
/// `summarizeCompaction`).
fn summarize_compaction(
    deps: &mut RegionDeps<'_>,
    session: &mut dyn SessionView,
    prepared: PreparedCompaction,
    compaction_id: &CompactionId,
    source_command_id: Option<&str>,
    rule: StabilityRule,
) -> Result<SummarizedCompaction, (CompactionFailure, Stage)> {
    let mut prepared = prepared;
    let result = loop {
        match (deps.summarize)(&prepared.input) {
            Ok(result) => break result,
            Err(error) => {
                // dsh rethrows an aborted signal before the stability
                // check; the abort carrier lands with the 17f wiring.
                assert_stable(rule, deps.meter, session, &prepared)
                    .map_err(|failure| (failure, Stage::Summary))?;
                if !(deps.recover)(&error, &prepared.selection.shadowed_seqs, session) {
                    return Err((error, Stage::Summary));
                }
                let selection = validate_surface_region(
                    session,
                    prepared.selection.start,
                    prepared.selection.end,
                )
                .map_err(|message| (CompactionFailure::Other(message), Stage::Summary))?;
                prepared = prepare_compaction(deps.meter, session, &selection)
                    .map_err(|failure| (failure, Stage::Summary))?;
            }
        }
    };
    let framed = frame_summary(&result.summary);
    let source_value = staged(encode(&compact_checkpoint_source(
        compaction_id.clone(),
        source_command_id.map(str::to_owned),
    )))?;
    let serde_json::Value::Object(source_map) = source_value else {
        return Err((
            CompactionFailure::Other(
                "the checkpoint source must serialize to an object".to_owned(),
            ),
            Stage::Summary,
        ));
    };
    let checkpoint_message = Message::Known(Box::new(KnownMessage::User {
        id: MessageId::new(Uuid::new_v4().to_string()),
        content: framed,
        source: MessageSource::Other(source_map),
        extra: RawObject::new(),
    }));
    // The checkpoint is text-only, so its fixed-heuristic price IS its
    // route price; comparing it against the span's route price asks the
    // real question — does the replacement lower the next request's
    // pressure.
    let framed_summary_token_count = deps.meter.estimate_message(&checkpoint_message);
    if framed_summary_token_count >= prepared.shadowed_route_token_count {
        return Err((
            CompactionFailure::Other(format!(
                "summary is not smaller than the shadowed content \
                 ({framed_summary_token_count} estimated framed tokens >= {})",
                prepared.shadowed_route_token_count
            )),
            Stage::Summary,
        ));
    }
    Ok(SummarizedCompaction {
        prepared,
        result,
        checkpoint_message,
    })
}

/// Reject a summary prepared against any earlier surface relationship (dsh
/// `assertWholeSurfaceUnchanged` / `assertSelectedSpanStable`).
fn assert_stable(
    rule: StabilityRule,
    meter: &dyn MeterView,
    session: &dyn SessionView,
    prepared: &PreparedCompaction,
) -> Result<(), CompactionFailure> {
    match rule {
        StabilityRule::WholeSurface => {
            let current = meter.measure(session).map_err(CompactionFailure::Other)?;
            if current.nodes != prepared.measurement.nodes {
                return Err(CompactionFailure::SurfaceChanged(
                    "compaction: session surface changed during summarization".to_owned(),
                ));
            }
        }
        StabilityRule::SelectedSpan => {
            let current =
                validate_surface_region(session, prepared.selection.start, prepared.selection.end)
                    .map_err(|_validation| {
                        CompactionFailure::SurfaceChanged(
                            "compaction: the selected span is no longer a valid replacement \
                     target"
                                .to_owned(),
                        )
                    })?;
            if current.shadowed_seqs != prepared.selection.shadowed_seqs {
                return Err(CompactionFailure::SurfaceChanged(
                    "compaction: the selected span changed during summarization".to_owned(),
                ));
            }
            let measured = meter
                .measure(session)
                .map_err(CompactionFailure::Other)?
                .nodes[current.start_idx..=current.end_idx]
                .to_vec();
            if measured != prepared.selected_nodes {
                return Err(CompactionFailure::SurfaceChanged(
                    "compaction: the selected span was rewritten during \
                     summarization"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Append one completed summary record and replacement body without
/// yielding (dsh `commitCompactionBody`).
fn commit_compaction_body(
    session: &mut dyn SessionView,
    start_seq: SessionSeq,
    compaction_id: &CompactionId,
    source_command_id: Option<&str>,
    summarized: &SummarizedCompaction,
) -> Result<PendingResult, String> {
    let selection = &summarized.prepared.selection;
    let summary_data = CompactionSummaryData {
        compaction_id: compaction_id.clone(),
        source_command_id: source_command_id.map(str::to_owned),
        summary: summarized.result.summary.clone(),
        shadowed_range: ShadowedRange {
            start: selection.start,
            end: selection.end,
        },
        shadowed_seqs: selection.shadowed_seqs.clone(),
        shadowed_token_count: summarized.prepared.shadowed_token_count,
        provider: summarized.result.provider.clone(),
        model: summarized.result.model.clone(),
        max_tokens: summarized.result.max_tokens,
        usage: summarized.result.usage,
        raw_output: summarized.result.raw_output.clone(),
        llm_stream_call: summarized
            .result
            .llm_stream_call
            .then_some(fr_wire::TrueOnly),
    };
    let summary_event = append_typed(
        session,
        crate::types::event_type::SUMMARY,
        encode(&summary_data)?,
        None,
        None,
    )?;
    session.append(AppendRequest {
        type_name: event_type::USER_MESSAGE,
        data: encode(&summarized.checkpoint_message)?,
        surface_op: Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: selection.start,
            end_seq: selection.end,
        })),
        source_event_seqs: Some(
            [start_seq, summary_event.seq]
                .into_iter()
                .chain(selection.shadowed_seqs.iter().copied())
                .collect(),
        ),
    })?;
    Ok(PendingResult {
        compaction_id: compaction_id.clone(),
        source_command_id: source_command_id.map(str::to_owned),
        start_seq,
        summary_seq: summary_event.seq,
        end_seq: start_seq,
        summary: summarized.result.summary.clone(),
        shadowed_range: ShadowedRange {
            start: selection.start,
            end: selection.end,
        },
        shadowed_seqs: selection.shadowed_seqs.clone(),
        shadowed_token_count: summarized.prepared.shadowed_token_count,
    })
}

/// Reconstruct the last routed request's cacheable prefix for the shadowed
/// region (dsh `buildSummarizationInput`): the system prompt held by the
/// `system/message` at surface node 0, the header's tool schemas, then the
/// region's own derived messages in surface order. The summarizer appends
/// only the compaction instruction after this, so the call is a genuine
/// prefix of the conversation and reuses the provider's KV cache.
fn build_summarization_input(
    session: &dyn SessionView,
    shadowed_seqs: &[SessionSeq],
) -> SummarizationInput {
    let header = session.request_header();
    let system = session
        .surface_nodes()
        .first()
        .copied()
        .and_then(|head_seq| system_head(session.events(), head_seq))
        .and_then(|event| derive_event_message(event).cloned());
    let region_messages = shadowed_seqs
        .iter()
        .filter_map(|seq| {
            // The projected value wins (the dsh `deriveEventMessage`
            // consults the committed projections first), else the raw
            // event's own derivation.
            if let Some(projected) = session.projected_message(*seq) {
                return Some(projected.clone());
            }
            event_at(session.events(), *seq)
                .and_then(derive_event_message)
                .cloned()
        })
        .collect::<Vec<Message>>();
    let mut messages = Vec::with_capacity(region_messages.len() + 1);
    if let Some(system) = system {
        messages.push(system);
    }
    messages.extend(region_messages);
    SummarizationInput {
        tools: header.and_then(|header| header.tools.clone()),
        messages,
    }
}

/// Serialize one event payload object.
fn encode<T: Serialize + ?Sized>(value: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|error| format!("compaction event encode failed: {error}"))
}

fn staged(
    result: Result<serde_json::Value, String>,
) -> Result<serde_json::Value, (CompactionFailure, Stage)> {
    result.map_err(|message| (CompactionFailure::Other(message), Stage::Summary))
}

/// Append one typed event with optional surface metadata.
fn append_typed(
    session: &mut dyn SessionView,
    type_name: &str,
    data: serde_json::Value,
    surface_op: Option<SurfaceOp>,
    source_event_seqs: Option<Vec<SessionSeq>>,
) -> Result<SessionEvent, String> {
    session.append(AppendRequest {
        type_name,
        data,
        surface_op,
        source_event_seqs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meter::MeasurementBaseline;
    use fr_session::fold_surface;
    use fr_wire::message::KnownMessageSource;
    use fr_wire::{AppendOp, KnownContentBlock, SessionEventData};
    use std::cell::RefCell;

    fn seq(value: u64) -> SessionSeq {
        SessionSeq::new(value).unwrap_or_else(|error| panic!("seq: {error}"))
    }

    fn text_block(value: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Text {
            text: value.to_owned(),
        })
    }

    fn user_message(text: &str) -> Message {
        Message::Known(Box::new(KnownMessage::User {
            id: MessageId::new(Uuid::new_v4().to_string()),
            content: vec![text_block(text)],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }))
    }

    fn assistant_message(text: &str) -> Message {
        Message::Known(Box::new(KnownMessage::Assistant {
            id: MessageId::new(Uuid::new_v4().to_string()),
            content: vec![text_block(text)],
            source: MessageSource::Known(KnownMessageSource::Model {
                provider: "mock".to_owned(),
                model: "mock".to_owned(),
                replay_state: None,
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }))
    }

    /// An in-memory session view whose appends run the real surface fold.
    struct FakeSession {
        events: Vec<SessionEvent>,
        nodes: Vec<SessionSeq>,
        generation: u64,
        header: Option<EpochHeader>,
    }

    impl FakeSession {
        fn new() -> Self {
            Self {
                events: Vec::new(),
                nodes: Vec::new(),
                generation: 0,
                header: None,
            }
        }

        fn append_local(&mut self, event_type: &str, data: serde_json::Value) {
            self.append(AppendRequest {
                type_name: event_type,
                data,
                surface_op: None,
                source_event_seqs: None,
            })
            .expect("append");
        }

        fn append_surface(&mut self, event_type: &str, data: serde_json::Value) {
            self.append(AppendRequest {
                type_name: event_type,
                data,
                surface_op: Some(SurfaceOp::Append(AppendOp)),
                source_event_seqs: None,
            })
            .expect("append surface");
        }

        fn open_turn(&mut self) {
            self.append_local("turn/start", serde_json::json!({"turn": 1}));
        }

        fn seed_conversation(&mut self, pairs: usize) {
            self.open_turn();
            self.seed_surface_only(pairs);
        }

        fn seed_surface_only(&mut self, pairs: usize) {
            for index in 0..pairs {
                self.append_surface(
                    "user/message",
                    serde_json::to_value(user_message(&format!("u{index}"))).expect("user"),
                );
                self.append_surface(
                    "assistant/message",
                    serde_json::json!({
                        "turn": 1, "step": index + 1,
                        "message": serde_json::to_value(assistant_message(&format!("a{index}")))
                            .expect("assistant"),
                        "stream": [],
                    }),
                );
            }
        }
    }

    impl SessionView for FakeSession {
        fn events(&self) -> &[SessionEvent] {
            &self.events
        }

        fn surface_nodes(&self) -> &[SessionSeq] {
            &self.nodes
        }

        fn replace_generation(&self) -> u64 {
            self.generation
        }

        fn request_header(&self) -> Option<&EpochHeader> {
            self.header.as_ref()
        }

        fn append(&mut self, request: AppendRequest<'_>) -> Result<SessionEvent, String> {
            let next = self.events.last().map_or(0, |event| event.seq.get() + 1);
            let seq = seq(next);
            let data = SessionEventData::from_parts(request.type_name, request.data)?;
            let event = SessionEvent {
                seq,
                time: i64::try_from(self.events.len()).unwrap_or(i64::MAX),
                data,
                ignorable: None,
                surface_op: request.surface_op,
                source_event_seqs: request.source_event_seqs,
            };
            self.events.push(event.clone());
            let folded = fold_surface(&self.events).map_err(|error| error.to_string())?;
            self.nodes = folded.nodes;
            self.generation = folded.replace_generation;
            Ok(event)
        }
    }

    /// A flat-pricing meter; the checkpoint estimate is a separate face so
    /// the shrink gate can be driven independently of the surface prices.
    struct FakeMeter {
        per_node: u64,
        checkpoint_estimate: u64,
        /// Price overrides that activate after the first measure (the
        /// rewrite-during-summarization probes).
        reprice_after_first_measure: bool,
        measures: RefCell<usize>,
    }

    impl FakeMeter {
        fn new(per_node: u64) -> Self {
            Self {
                per_node,
                checkpoint_estimate: 20,
                reprice_after_first_measure: false,
                measures: RefCell::new(0),
            }
        }
    }

    impl MeterView for FakeMeter {
        fn measure(&self, session: &dyn SessionView) -> Result<SurfaceMeasurement, String> {
            let mut count = self.measures.borrow_mut();
            let reprice = self.reprice_after_first_measure && *count >= 1;
            *count += 1;
            drop(count);
            let nodes = session
                .surface_nodes()
                .iter()
                .map(|seq| MeterSurfaceNode {
                    seq: seq.get(),
                    tokens: if reprice {
                        self.per_node + 1
                    } else {
                        self.per_node
                    },
                    heuristic_tokens: self.per_node,
                })
                .collect();
            let total = self.per_node * u64::try_from(session.surface_nodes().len()).unwrap_or(0);
            Ok(SurfaceMeasurement {
                log_revision: u64::try_from(session.events().len()).unwrap_or(0),
                baseline: MeasurementBaseline::None { tokens: 0 },
                surface_delta_tokens: 0,
                total_tokens: total,
                surface_tokens: total,
                nodes,
            })
        }

        fn estimate_message(&self, _message: &Message) -> u64 {
            self.checkpoint_estimate
        }
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

    fn ok_summarize(
        text: &'static str,
    ) -> impl FnMut(&SummarizationInput) -> Result<SummaryResult, CompactionFailure> {
        move |_| Ok(summary_of(text))
    }

    fn no_recover() -> impl FnMut(&CompactionFailure, &[SessionSeq], &mut dyn SessionView) -> bool {
        |_, _, _| false
    }

    #[test]
    fn selects_after_a_system_head_and_keeps_it_out_of_the_range() {
        let mut session = FakeSession::new();
        session.append_surface(
            "system/message",
            serde_json::json!({
                "turn": 1, "step": 0,
                "message": serde_json::to_value(user_message("SYSTEM HEAD")).expect("system"),
            }),
        );
        session.seed_conversation(4);
        let meter = FakeMeter::new(100);
        let measurement = meter.measure(&session).expect("measure");
        assert_eq!(measurement.nodes.len(), 9);
        // Retain the last two nodes: the range starts after the head.
        let range = select_compactable_range(&session, &measurement, 200).expect("range");
        let (start, end) = range.expect("some range");
        assert_eq!(start, seq(2));
        assert_eq!(end, session.surface_nodes()[6]);
    }

    /// A retained-tail boundary that would split an assistant
    /// tool-call/result pair walks back to the balanced position, and
    /// the range never includes the node-0 system head.
    #[test]
    fn selection_falls_back_to_the_balanced_pair_boundary() {
        let mut session = FakeSession::new();
        session.append_surface(
            "system/message",
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": serde_json::to_value(user_message("SYSTEM HEAD")).expect("system"),
            }),
        );
        session.seed_surface_only(1);
        session.append_surface(
            "assistant/message",
            serde_json::json!({
                "turn": 1, "step": 2,
                "message": {"role": "assistant", "id": "a-call", "content": [
                    {"type": "tool-call", "id": "c1", "name": "bash", "arguments": "{}"}
                ], "source": {"kind": "model", "provider": "mock", "model": "mock"}},
                "stream": [],
            }),
        );
        session.append_surface(
            "tool/result",
            serde_json::json!({
                "turn": 1, "step": 2,
                "message": {"role": "tool", "id": "t", "content": [], "toolCallId": "c1",
                            "source": {"kind": "tool", "callId": "c1"}},
            }),
        );
        let meter = FakeMeter::new(100);
        let measurement = meter.measure(&session).expect("measure");
        let (start, end) = select_compactable_range(&session, &measurement, 100)
            .expect("range")
            .expect("some range");
        let nodes = session.surface_nodes();
        assert_eq!(
            start, nodes[1],
            "the range starts after the system head (node-0 never rides)"
        );
        assert_eq!(
            end, nodes[2],
            "the boundary fell back before the tool pair (the keep keeps it whole)"
        );
    }

    #[test]
    fn declines_when_only_the_system_head_precedes_the_retained_tail() {
        let mut session = FakeSession::new();
        session.append_surface(
            "system/message",
            serde_json::json!({
                "turn": 1, "step": 0,
                "message": serde_json::to_value(user_message("SYSTEM HEAD")).expect("system"),
            }),
        );
        session.append_surface(
            "user/message",
            serde_json::to_value(user_message("u0")).expect("user"),
        );
        session.append_surface(
            "assistant/message",
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": serde_json::to_value(assistant_message("a0")).expect("assistant"),
                "stream": [],
            }),
        );
        let meter = FakeMeter::new(100);
        let measurement = meter.measure(&session).expect("measure");
        assert!(
            select_compactable_range(&session, &measurement, 200)
                .expect("range")
                .is_none()
        );
    }

    #[test]
    fn rejects_a_priced_surface_that_is_not_the_current_positional_surface() {
        let mut session = FakeSession::new();
        session.seed_conversation(2);
        let meter = FakeMeter::new(100);
        let mut measurement = meter.measure(&session).expect("measure");
        measurement.nodes.remove(0);
        let error = select_compactable_range(&session, &measurement, 1).expect_err("mismatch");
        assert!(error.contains("does not match"), "{error}");
    }

    #[test]
    fn validates_region_boundaries_verbatim() {
        let mut session = FakeSession::new();
        session.seed_conversation(2);
        let missing =
            validate_surface_region(&session, seq(99), seq(99)).expect_err("missing start");
        assert!(
            missing.contains("start seq 99 not found in surface"),
            "{missing}"
        );
        let reversed = validate_surface_region(&session, seq(4), seq(1)).expect_err("reversed");
        assert!(reversed.contains("is after end seq"), "{reversed}");
        let ok = validate_surface_region(&session, seq(1), seq(4)).expect("ok");
        assert_eq!(ok.shadowed_seqs, vec![seq(1), seq(2), seq(3), seq(4)]);
    }

    #[test]
    fn rejects_an_open_tool_pair_at_either_boundary() {
        let mut session = FakeSession::new();
        session.open_turn();
        session.append_surface(
            "assistant/message",
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": {"role": "assistant", "id": "a", "content": [
                    {"type": "tool-call", "id": "c1", "name": "bash", "arguments": "{}"}
                ], "source": {"kind": "model", "provider": "mock", "model": "mock"}},
                "stream": [],
            }),
        );
        session.append_surface(
            "tool/result",
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": {"role": "tool", "id": "t", "content": [], "toolCallId": "c1",
                            "source": {"kind": "tool", "callId": "c1"}},
            }),
        );
        session.append_surface(
            "assistant/message",
            serde_json::json!({
                "turn": 1, "step": 2,
                "message": serde_json::to_value(assistant_message("after")).expect("assistant"),
                "stream": [],
            }),
        );
        // The cut before the result splits the pair.
        let error =
            validate_surface_region(&session, seq(2), seq(3)).expect_err("unbalanced start");
        assert!(
            error.contains("would split a step's tool-call/result pair"),
            "{error}"
        );
    }

    #[test]
    fn busy_lock_and_seed_exemption() {
        let mut session = FakeSession::new();
        session.open_turn();
        session.append_local(
            "compaction/start",
            serde_json::json!({"compactionId": "t1", "turn": 1}),
        );
        let error = assert_no_active_compaction(&session, "compaction").expect_err("locked");
        assert_eq!(error.code, ManualCompactionErrorCode::Busy);
        assert!(
            error
                .message()
                .contains("compaction: compaction already in progress"),
            "{}",
            error.message()
        );
        // A later constructor seed proves the owner belongs to an earlier
        // lifecycle.
        session.append_local("session/end-seed", serde_json::json!({"header": {}}));
        assert!(
            assert_no_active_compaction(&session, "compaction").is_ok(),
            "the later seed boundary exempts the stale owner"
        );
    }

    #[test]
    fn commits_the_full_bracket_and_collapses_the_surface() {
        let mut session = FakeSession::new();
        session.seed_conversation(4);
        let meter = FakeMeter::new(100);
        let mut summarize = ok_summarize("the summary");
        let mut recover = no_recover();
        let mut deps = RegionDeps {
            meter: &meter,
            summarize: &mut summarize,
            recover: &mut recover,
        };
        let range = select_compactable_range(&session, &meter.measure(&session).expect("m"), 200)
            .expect("range")
            .expect("some range");
        let result = compact_surface_region(
            &mut deps,
            &mut session,
            range.0,
            range.1,
            &mut TransactionOptions {
                owner: BracketOwner::CurrentTurn,
                stability: StabilityRule::WholeSurface,
                flush: None,
                source_command_id: None,
            },
        )
        .expect("compaction");
        let types: Vec<&str> = session
            .events()
            .iter()
            .skip(1)
            .map(fr_wire::SessionEvent::event_type)
            .collect();
        // turn/start + 8 surface events, then the bracket: start, summary,
        // replacement, end.
        let tail = &types[types.len() - 4..];
        assert_eq!(
            tail,
            [
                "compaction/start",
                "compaction/summary",
                "user/message",
                "compaction/end"
            ]
        );
        assert_eq!(session.surface_nodes().len(), 3);
        assert_eq!(result.start_seq, seq(9));
        assert_eq!(result.summary_seq, seq(10));
        assert_eq!(result.end_seq, seq(12));
        assert_eq!(result.shadowed_seqs.len(), 6);
        // The replacement message carries the checkpoint framing and the
        // checkpoint source.
        let replacement = &session.events()[11];
        let SessionEventData::UserMessage(message) = &replacement.data else {
            panic!("the replacement is a user message");
        };
        let Message::Known(known) = message else {
            panic!("the replacement is a known message");
        };
        let KnownMessage::User {
            content, source, ..
        } = known.as_ref()
        else {
            panic!("the replacement is user-role");
        };
        assert_eq!(content.len(), 3);
        let SessionEventData::Unknown(start) = &session.events()[9].data else {
            panic!("the bracket opener is a plugin event");
        };
        assert_eq!(
            start.data["compactionId"],
            serde_json::json!(result.compaction_id.0)
        );
        assert_eq!(start.data["turn"], serde_json::json!(1));
        let MessageSource::Other(replacement_source) = source else {
            panic!("the replacement carries the marker source");
        };
        assert_eq!(
            replacement_source["compactionId"],
            serde_json::json!(result.compaction_id.0)
        );
        // The replacement cites the bracket pair plus every shadowed node.
        let cited = replacement.source_event_seqs.as_ref().expect("cited");
        assert_eq!(cited[0], result.start_seq);
        assert_eq!(cited[1], result.summary_seq);
        assert_eq!(cited.len(), 2 + result.shadowed_seqs.len());
        // A start whose close never lands stays detectable (the busy
        // guard refuses a fresh transaction over the orphan owner).
        let mut orphan = FakeSession::new();
        orphan.append_local(
            "compaction/start",
            serde_json::json!({"compactionId": "orphan", "turn": 1}),
        );
        let error = assert_no_active_compaction(&orphan, "compaction").expect_err("orphan start");
        assert_eq!(error.code, ManualCompactionErrorCode::Busy);
    }

    #[test]
    fn shrink_gate_rejection_closes_the_bracket_with_the_error() {
        let mut session = FakeSession::new();
        session.seed_conversation(2);
        let mut meter = FakeMeter::new(100);
        meter.checkpoint_estimate = 10_000;
        let mut summarize = ok_summarize("big summary");
        let mut recover = no_recover();
        let mut deps = RegionDeps {
            meter: &meter,
            summarize: &mut summarize,
            recover: &mut recover,
        };
        let error = compact_surface_region(
            &mut deps,
            &mut session,
            seq(1),
            seq(4),
            &mut TransactionOptions {
                owner: BracketOwner::CurrentTurn,
                stability: StabilityRule::WholeSurface,
                flush: None,
                source_command_id: None,
            },
        )
        .expect_err("shrink gate");
        let CompactionError::Failure(failure) = &error else {
            panic!("the automatic path rethrows the failure");
        };
        assert!(
            failure.message().contains(
                "summary is not smaller than the shadowed content (10000 estimated \
                 framed tokens >= 400)"
            ),
            "{}",
            failure.message()
        );
        // The close attempt recorded the verbatim failure; the start is
        // paired by its error-bearing end.
        let last = session.events().last().expect("end");
        assert_eq!(last.event_type(), "compaction/end");
        let SessionEventData::Unknown(end) = &last.data else {
            panic!("the closer is a plugin event");
        };
        assert!(
            end.data["error"]
                .as_str()
                .expect("error text")
                .contains("summary is not smaller"),
            "{}",
            end.data["error"]
        );
    }

    #[test]
    fn recovery_hook_reprepares_and_retries() {
        let mut session = FakeSession::new();
        session.seed_conversation(2);
        let meter = FakeMeter::new(100);
        let calls = RefCell::new(0);
        let mut summarize = |_: &SummarizationInput| -> Result<SummaryResult, CompactionFailure> {
            let mut count = calls.borrow_mut();
            *count += 1;
            if *count == 1 {
                Err(CompactionFailure::Llm {
                    message: "provider hiccup".to_owned(),
                    code: Some("PROVIDER_ERROR".to_owned()),
                    failure: None,
                })
            } else {
                Ok(summary_of("recovered summary"))
            }
        };
        let recovered = RefCell::new(0);
        let mut recover =
            |_: &CompactionFailure, _: &[SessionSeq], _: &mut dyn SessionView| -> bool {
                *recovered.borrow_mut() += 1;
                true
            };
        let mut deps = RegionDeps {
            meter: &meter,
            summarize: &mut summarize,
            recover: &mut recover,
        };
        let result = compact_surface_region(
            &mut deps,
            &mut session,
            seq(1),
            seq(4),
            &mut TransactionOptions {
                owner: BracketOwner::CurrentTurn,
                stability: StabilityRule::WholeSurface,
                flush: None,
                source_command_id: None,
            },
        )
        .expect("recovered");
        assert_eq!(*calls.borrow(), 2);
        assert_eq!(*recovered.borrow(), 1);
        assert_eq!(result.summary, vec![text_block("recovered summary")]);
    }

    #[test]
    fn manual_path_classifies_surface_changes_as_changed() {
        let mut session = FakeSession::new();
        session.seed_surface_only(2);
        let mut meter = FakeMeter::new(100);
        meter.reprice_after_first_measure = true;
        let mut summarize = ok_summarize("summary");
        let mut recover = no_recover();
        let mut deps = RegionDeps {
            meter: &meter,
            summarize: &mut summarize,
            recover: &mut recover,
        };
        let error = compact_surface_region(
            &mut deps,
            &mut session,
            seq(0),
            seq(3),
            &mut TransactionOptions {
                owner: BracketOwner::Standalone,
                stability: StabilityRule::SelectedSpan,
                flush: None,
                source_command_id: Some("cmd-1".to_owned()),
            },
        )
        .expect_err("rewritten");
        let CompactionError::Manual(manual) = &error else {
            panic!("the manual path classifies");
        };
        assert_eq!(manual.code, ManualCompactionErrorCode::Changed);
        assert_eq!(
            manual.message(),
            "the compacted history changed during manual compaction"
        );
    }

    #[test]
    fn manual_path_classifies_summary_failures_and_open_turn_rejects_standalone() {
        let mut session = FakeSession::new();
        session.seed_surface_only(2);
        let meter = FakeMeter::new(100);
        let mut summarize = |_: &SummarizationInput| -> Result<SummaryResult, CompactionFailure> {
            Err(CompactionFailure::Llm {
                message: "provider down".to_owned(),
                code: None,
                failure: None,
            })
        };
        let mut recover = no_recover();
        let mut deps = RegionDeps {
            meter: &meter,
            summarize: &mut summarize,
            recover: &mut recover,
        };
        let error = compact_surface_region(
            &mut deps,
            &mut session,
            seq(0),
            seq(3),
            &mut TransactionOptions {
                owner: BracketOwner::Standalone,
                stability: StabilityRule::SelectedSpan,
                flush: None,
                source_command_id: None,
            },
        )
        .expect_err("summary failure");
        let CompactionError::Manual(manual) = &error else {
            panic!("classified");
        };
        assert_eq!(manual.code, ManualCompactionErrorCode::Summary);
        assert_eq!(
            manual.message(),
            "manual compaction could not produce a smaller summary"
        );

        // A standalone bracket inside an open turn is busy.
        let mut turn_session = FakeSession::new();
        turn_session.seed_conversation(2);
        let meter = FakeMeter::new(100);
        let mut summarize = ok_summarize("s");
        let mut recover = no_recover();
        let mut deps = RegionDeps {
            meter: &meter,
            summarize: &mut summarize,
            recover: &mut recover,
        };
        let error = compact_surface_region(
            &mut deps,
            &mut turn_session,
            seq(2),
            seq(4),
            &mut TransactionOptions {
                owner: BracketOwner::Standalone,
                stability: StabilityRule::SelectedSpan,
                flush: None,
                source_command_id: None,
            },
        )
        .expect_err("open turn");
        let CompactionError::Manual(manual) = &error else {
            panic!("classified");
        };
        assert_eq!(manual.code, ManualCompactionErrorCode::Busy);
        assert_eq!(
            manual.message(),
            "manual compaction: the session already has an open turn"
        );
    }

    #[test]
    fn flush_failure_classifies_as_persistence() {
        let mut session = FakeSession::new();
        session.seed_surface_only(2);
        let meter = FakeMeter::new(100);
        let mut summarize = ok_summarize("s");
        let mut recover = no_recover();
        let mut deps = RegionDeps {
            meter: &meter,
            summarize: &mut summarize,
            recover: &mut recover,
        };
        let mut flush = || -> Result<(), String> { Err("disk full".to_owned()) };
        let error = compact_surface_region(
            &mut deps,
            &mut session,
            seq(0),
            seq(3),
            &mut TransactionOptions {
                owner: BracketOwner::Standalone,
                stability: StabilityRule::SelectedSpan,
                flush: Some(&mut flush),
                source_command_id: None,
            },
        )
        .expect_err("flush");
        let CompactionError::Manual(manual) = &error else {
            panic!("classified");
        };
        assert_eq!(manual.code, ManualCompactionErrorCode::Persistence);
        assert_eq!(
            manual.message(),
            "manual compaction durability checkpoint failed"
        );
    }

    #[test]
    fn the_automatic_path_requires_an_open_turn() {
        let mut session = FakeSession::new();
        session.seed_surface_only(2);
        let meter = FakeMeter::new(100);
        let mut summarize = ok_summarize("s");
        let mut recover = no_recover();
        let mut deps = RegionDeps {
            meter: &meter,
            summarize: &mut summarize,
            recover: &mut recover,
        };
        let error = compact_surface_region(
            &mut deps,
            &mut session,
            seq(0),
            seq(3),
            &mut TransactionOptions {
                owner: BracketOwner::CurrentTurn,
                stability: StabilityRule::WholeSurface,
                flush: None,
                source_command_id: None,
            },
        )
        .expect_err("no turn");
        let CompactionError::Failure(failure) = &error else {
            panic!("verbatim rethrow");
        };
        assert!(
            failure
                .message()
                .contains("compactRegion: no open turn — automatic compaction events"),
            "{}",
            failure.message()
        );
    }
}
