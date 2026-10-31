// Rust guideline compliant 2026-10-31
//! The session-query engine (dsh `@deepseek-ai/dsh-session-query`
//! 1:1): exact reads, filters, and traces over the live-preferred
//! logical corpus.
//!
//! Q16d-2A pins the search-disabled terminal state (the dsh-base
//! `openAt: 'never'` semantics): exact reads, titles, and lineage
//! traces stay available over the live-preferred corpus while
//! `searchSessions` and `searchEvents` fail with
//! `SESSION_QUERY_SEARCH_DISABLED`.
//!
//! The corpus consumes the wire-backed source seam: the live read face
//! answers the current session, the persisted faces answer stored
//! sessions, and the projection relay answers the turn-boundary reads.
//! The dsh `ctx.sessions.list()` enumeration has no wire equivalent in
//! M1, so the corpus's live overlay takes the live set as an argument
//! (M1 callers pass the one live session; Phase 18 widens it to the
//! registry).

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use fr_wire::Message;
use fr_wire::content::{ContentBlock, KnownContentBlock};
use fr_wire::event::{SessionEvent, SessionEventData, TurnEndReasonKind};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Error taxonomy
// ---------------------------------------------------------------------------

/// The closed machine-routable failure taxonomy (dsh
/// `SessionQueryErrorCode`; the 17-member closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionQueryErrorCode {
    /// The read was cancelled.
    Aborted,
    /// The stored log failed structural validation.
    CorruptSession,
    /// The target event does not exist.
    EventNotFound,
    /// The search index failed to open or update.
    IndexFailed,
    /// A configuration row failed its check.
    InvalidConfig,
    /// A continuation cursor repeated.
    InvalidCursor,
    /// A filter clause failed validation.
    InvalidFilter,
    /// A page-size row failed validation.
    InvalidLimit,
    /// A query text failed normalization.
    InvalidQuery,
    /// A lineage walk cycled.
    InvalidLineage,
    /// A surface fold failed.
    InvalidSurface,
    /// A read window exceeded the configured bound.
    InvalidWindow,
    /// The persistence layer failed.
    PersistenceFailed,
    /// The deployment disabled full-text search.
    SearchDisabled,
    /// The target session does not exist.
    SessionNotFound,
    /// A cursor predates the current index generation.
    StaleCursor,
    /// Two observations of one logical source conflict.
    SourceConflict,
}

impl SessionQueryErrorCode {
    /// The wire code word (the dsh code string).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aborted => "SESSION_QUERY_ABORTED",
            Self::CorruptSession => "SESSION_QUERY_CORRUPT_SESSION",
            Self::EventNotFound => "SESSION_QUERY_EVENT_NOT_FOUND",
            Self::IndexFailed => "SESSION_QUERY_INDEX_FAILED",
            Self::InvalidConfig => "SESSION_QUERY_INVALID_CONFIG",
            Self::InvalidCursor => "SESSION_QUERY_INVALID_CURSOR",
            Self::InvalidFilter => "SESSION_QUERY_INVALID_FILTER",
            Self::InvalidLimit => "SESSION_QUERY_INVALID_LIMIT",
            Self::InvalidQuery => "SESSION_QUERY_INVALID_QUERY",
            Self::InvalidLineage => "SESSION_QUERY_INVALID_LINEAGE",
            Self::InvalidSurface => "SESSION_QUERY_INVALID_SURFACE",
            Self::InvalidWindow => "SESSION_QUERY_INVALID_WINDOW",
            Self::PersistenceFailed => "SESSION_QUERY_PERSISTENCE_FAILED",
            Self::SearchDisabled => "SESSION_QUERY_SEARCH_DISABLED",
            Self::SessionNotFound => "SESSION_QUERY_SESSION_NOT_FOUND",
            Self::StaleCursor => "SESSION_QUERY_STALE_CURSOR",
            Self::SourceConflict => "SESSION_QUERY_SOURCE_CONFLICT",
        }
    }
}

/// The typed session-query failure (dsh `SessionQueryError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionQueryError {
    /// The closed taxonomy code.
    pub code: SessionQueryErrorCode,
    /// The failure text.
    pub message: String,
}

impl SessionQueryError {
    /// Builds one failure over the closed code.
    #[must_use]
    pub fn new(code: SessionQueryErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The wire body-error shape (the `SessionQueryError` name carries).
    #[must_use]
    pub fn to_body(&self) -> crate::tools::BodyError {
        crate::tools::BodyError {
            code: Some(self.code.as_str().to_owned()),
            name: "SessionQueryError".to_owned(),
            message: self.message.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Default maximum `before`/`after` raw-event window (dsh
/// `SESSION_QUERY_READ_WINDOW_MAX`).
pub const READ_WINDOW_MAX: u64 = 50;
/// Default maximum concurrent persisted-log reads in one batch read.
pub const DEFAULT_PERSISTED_READ_CONCURRENCY: u64 = 4;
/// Default maximum cold prepared-Session observations retained (the dsh
/// constant; the wire-backed reader re-reads per call in M1, so this
/// documents the inherited bound).
pub const DEFAULT_PREPARED_SESSION_CACHE_SIZE: u64 = 5;

/// The backend-independent engine configuration (dsh `Config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Maximum accepted raw read context on either side.
    pub read_window_max: u64,
    /// Maximum concurrent persisted-log reads in one batch read.
    pub persisted_read_concurrency: u64,
    /// Maximum cold prepared-Session observations retained for reuse.
    pub prepared_session_cache_size: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            read_window_max: READ_WINDOW_MAX,
            persisted_read_concurrency: DEFAULT_PERSISTED_READ_CONCURRENCY,
            prepared_session_cache_size: DEFAULT_PREPARED_SESSION_CACHE_SIZE,
        }
    }
}

impl EngineConfig {
    /// Resolves the config with the dsh validation texts verbatim.
    ///
    /// The dsh rows accept `undefined` (the defaults); the fr composed
    /// loader validates the same rows through the fr-schema declaration,
    /// so this resolver pins the engine-side texts for the direct
    /// constructors and the tests.
    ///
    /// # Errors
    ///
    /// The three dsh `SESSION_QUERY_INVALID_CONFIG` texts, verbatim.
    pub fn resolve(
        read_window_max: Option<u64>,
        persisted_read_concurrency: Option<u64>,
        prepared_session_cache_size: Option<u64>,
    ) -> Result<Self, SessionQueryError> {
        let read_window_max = read_window_max.unwrap_or(READ_WINDOW_MAX);
        if read_window_max > i64::MAX as u64 {
            // The dsh `Number.isInteger` bound; the u64 domain cannot
            // carry a negative, so only the non-integer arm's failure
            // family remains reachable through the loader row.
            return Err(SessionQueryError::new(
                SessionQueryErrorCode::InvalidConfig,
                "session-query: readWindowMax must be a non-negative integer",
            ));
        }
        let persisted_read_concurrency =
            persisted_read_concurrency.unwrap_or(DEFAULT_PERSISTED_READ_CONCURRENCY);
        if persisted_read_concurrency == 0 || persisted_read_concurrency > 9_007_199_254_740_991 {
            return Err(SessionQueryError::new(
                SessionQueryErrorCode::InvalidConfig,
                "session-query: persistedReadConcurrency must be a positive safe integer",
            ));
        }
        let prepared_session_cache_size =
            prepared_session_cache_size.unwrap_or(DEFAULT_PREPARED_SESSION_CACHE_SIZE);
        if prepared_session_cache_size == 0 || prepared_session_cache_size > 9_007_199_254_740_991 {
            return Err(SessionQueryError::new(
                SessionQueryErrorCode::InvalidConfig,
                "session-query: preparedSessionCacheSize must be a positive safe integer",
            ));
        }
        Ok(Self {
            read_window_max,
            persisted_read_concurrency,
            prepared_session_cache_size,
        })
    }
}

// ---------------------------------------------------------------------------
// Wire shapes and the source seam
// ---------------------------------------------------------------------------

/// One detached log the source seam answers (the wire shape: header,
/// inherited prefix, and raw events in log order).
#[derive(Debug, Clone)]
pub struct QueryLog {
    /// The cloned source header.
    pub header: Value,
    /// The exact fork-inherited event count paired with the header.
    pub inherited_event_count: u64,
    /// The raw event objects in log order.
    pub events: Vec<Value>,
}

/// A decoded log: the header plus the typed, validated event list (the
/// corpus read shape the engine folds over).
#[derive(Debug, Clone)]
pub struct EngineLog {
    /// The cloned source header.
    pub header: Value,
    /// The exact fork-inherited event count paired with the header.
    pub inherited_event_count: u64,
    /// The typed events in log order (index == seq, the contiguous-log
    /// invariant the corpus validates on decode).
    pub events: Vec<SessionEvent>,
}

/// The source seam (the dsh `ctx.sessions` + optional
/// `sessionPersistence` binding, wire-backed).
pub trait QueryWire: Send + Sync {
    /// Reads one live session's log (`Ok(None)` when the id is not
    /// live; the dsh `ctx.sessions.get`).
    ///
    /// # Errors
    ///
    /// The live read's transport family (the dsh in-process get cannot
    /// fail; the wire face can).
    fn session_read(&self, id: &str) -> Result<Option<QueryLog>, String>;

    /// Lists the stored sessions as header rows (the dsh
    /// `persistence.list`).
    ///
    /// # Errors
    ///
    /// The listing transport family.
    fn persisted_list(&self) -> Result<Vec<Value>, String>;

    /// Reads one stored session's balanced cold log (`Ok(None)` when no
    /// stored log exists; the dsh `readColdSessionLog`).
    ///
    /// # Errors
    ///
    /// The persistence transport family (corruption carries the
    /// `SessionPersistenceCorruptionError` text prefix).
    fn persisted_cold_read(&self, id: &str) -> Result<Option<QueryLog>, String>;

    /// Reads one registered projection through the relay (`None` when
    /// the key is unregistered; the dsh `stateOf`).
    ///
    /// # Errors
    ///
    /// The relay or decode refusal text.
    fn projection_state(&self, session: &str, key: &str) -> Result<Option<Value>, String>;
}

/// Decodes one wire log into the typed shape (the contiguous-log
/// decode: events parse and land at their seq index).
///
/// # Errors
///
/// The decode refusal text (the caller classifies it into the query
/// taxonomy).
pub fn decode_log(log: QueryLog) -> Result<EngineLog, String> {
    let events = serde_json::from_value::<Vec<SessionEvent>>(Value::Array(log.events))
        .map_err(|error| format!("stored session events decode failed: {error}"))?;
    Ok(EngineLog {
        header: log.header,
        inherited_event_count: log.inherited_event_count,
        events,
    })
}

// ---------------------------------------------------------------------------
// Shared header checks (dsh sources.ts)
// ---------------------------------------------------------------------------

/// Reads one header field as text.
#[must_use]
pub fn header_id(header: &Value) -> Option<&str> {
    header.get("id").and_then(Value::as_str)
}

/// Reads one header's creation time (epoch milliseconds).
#[must_use]
pub fn header_created_at(header: &Value) -> Option<f64> {
    header.get("createdAt").and_then(Value::as_f64)
}

/// Reads one header's working directory.
#[must_use]
pub fn header_cwd(header: &Value) -> Option<&str> {
    header.get("cwd").and_then(Value::as_str)
}

/// Reads one header's parent session id.
#[must_use]
pub fn header_parent(header: &Value) -> Option<&str> {
    header.get("parentSession").and_then(Value::as_str)
}

/// Reads one header's seeded flag (absent = the dsh `false`).
#[must_use]
pub fn header_is_seeded(header: &Value) -> bool {
    header
        .get("isSeeded")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Reads one header's delegation depth (absent = the dsh `?? 0`; the
/// integer domain carries the exact equality the header check needs).
#[must_use]
pub fn header_delegation_depth(header: &Value) -> i64 {
    header
        .get("delegationDepth")
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// Rejects incompatible observations of one logical session source
/// (dsh `assertSessionHeadersCompatible`, verbatim fields and text).
///
/// # Errors
///
/// The `SESSION_QUERY_SOURCE_CONFLICT` refusal.
pub fn assert_session_headers_compatible(a: &Value, b: &Value) -> Result<(), SessionQueryError> {
    let same_id = header_id(a) == header_id(b);
    let same_created = header_created_at(a) == header_created_at(b);
    let same_cwd = header_cwd(a) == header_cwd(b);
    let same_parent = header_parent(a) == header_parent(b);
    let same_seeded = header_is_seeded(a) == header_is_seeded(b);
    let same_depth = header_delegation_depth(a) == header_delegation_depth(b);
    if same_id && same_created && same_cwd && same_parent && same_seeded && same_depth {
        return Ok(());
    }
    Err(SessionQueryError::new(
        SessionQueryErrorCode::SourceConflict,
        format!(
            "session source headers conflict for session \"{}\"",
            header_id(a).unwrap_or_default()
        ),
    ))
}

// ---------------------------------------------------------------------------
// Semantic text extraction (dsh extraction.ts)
// ---------------------------------------------------------------------------

/// Joins trimmed non-empty parts with newlines (dsh `joinText`).
#[must_use]
pub fn join_text(parts: Vec<String>) -> String {
    parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One content block's semantic texts (dsh `blockText`: reasoning and
/// unknown shapes contribute nothing).
#[must_use]
pub fn block_text(block: &ContentBlock) -> Vec<String> {
    match block {
        ContentBlock::Known(KnownContentBlock::Text { text }) => vec![text.clone()],
        ContentBlock::Known(KnownContentBlock::ToolCall {
            name, arguments, ..
        }) => {
            vec![name.clone(), arguments.clone()]
        }
        _ => Vec::new(),
    }
}

/// One message's content blocks (the `Message` enum's known-role read).
#[must_use]
pub fn message_content(message: &Message) -> &[ContentBlock] {
    match message {
        Message::Known(known) => known.content(),
        Message::Other(_) => &[],
    }
}

/// One message's semantic texts (dsh `contentText`).
#[must_use]
pub fn content_text(content: &[ContentBlock]) -> String {
    join_text(content.iter().flat_map(block_text).collect::<Vec<_>>())
}

/// One `turn/end` reason's semantic text (dsh `turnEndText`).
#[must_use]
pub fn turn_end_text(reason: &fr_wire::event::TurnEndReason) -> String {
    match reason {
        fr_wire::event::TurnEndReason::Known(TurnEndReasonKind::Error { error, .. }) => {
            join_text(vec!["error".to_owned(), error.message.clone()])
        }
        fr_wire::event::TurnEndReason::Known(TurnEndReasonKind::Aborted { .. }) => {
            "aborted".to_owned()
        }
        fr_wire::event::TurnEndReason::Known(TurnEndReasonKind::MaxTokens) => {
            "max-tokens".to_owned()
        }
        fr_wire::event::TurnEndReason::Known(TurnEndReasonKind::Interrupted) => {
            "interrupted".to_owned()
        }
        _ => String::new(),
    }
}

/// Extracts the searchable semantic text from one first-party session
/// event (dsh `extractSessionEventText`).
///
/// Structural boundaries, embedded raw streams, request envelopes, and
/// unknown events contribute no text; the `todo/write` snapshot rides
/// the unknown-type retention.
#[must_use]
pub fn extract_session_event_text(event: &SessionEvent) -> String {
    match &event.data {
        SessionEventData::UserMessage(message) => content_text(message_content(message)),
        SessionEventData::AssistantMessage(data) => content_text(message_content(&data.message)),
        SessionEventData::ToolCall(data) => {
            join_text(vec![data.name.clone(), data.arguments.clone()])
        }
        SessionEventData::ToolResult(data) => join_text(vec![
            content_text(message_content(&data.message)),
            data.error
                .as_ref()
                .map(|error| error.name.clone())
                .unwrap_or_default(),
            data.error
                .as_ref()
                .map(|error| error.code.clone())
                .unwrap_or_default(),
        ]),
        SessionEventData::Unknown(unknown) if unknown.type_name == "todo/write" => {
            let todos = unknown
                .data
                .get("todos")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let parts = todos
                .iter()
                .flat_map(|todo| {
                    vec![
                        todo.get("status")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        todo.get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    ]
                })
                .collect::<Vec<_>>();
            join_text(parts)
        }
        SessionEventData::TurnEnd(data) => turn_end_text(&data.reason),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Tracing helpers (dsh tracing.ts)
// ---------------------------------------------------------------------------

/// One event's lightweight record (dsh `SessionEventRecord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    /// Session that owns the event.
    pub session_id: String,
    /// Monotonic event seq within the session.
    pub seq: u64,
    /// Discriminant of the session event.
    pub event_type: String,
    /// Event timestamp in Unix epoch milliseconds.
    pub time: i64,
    /// Event placement in the folded session surface.
    pub surface: &'static str,
}

/// One event's direct positional replacements and relationships (dsh
/// `SessionEventTrace`).
#[derive(Debug, Clone)]
pub struct EventTrace {
    /// Lightweight target record.
    pub target: EventRecord,
    /// Immediate positional replacement event, when the target was
    /// shadowed.
    pub replaced_by: Option<u64>,
    /// Positional replacers from the immediate replacement to the final.
    pub replacement_chain: Vec<u64>,
    /// Surface nodes directly removed when the target itself performed
    /// a replacement.
    pub replaced_event_seqs: Vec<u64>,
    /// Earlier events cited directly as sources, in recorded order.
    pub source_event_seqs: Vec<u64>,
    /// Later events that directly cite the target as a source, in log
    /// order.
    pub derived_event_seqs: Vec<u64>,
}

/// One surface-fold analysis of a complete raw log (dsh
/// `analyzeEventLog`).
struct EventLogAnalysis {
    records: Vec<EventRecord>,
    replaced_by: HashMap<u64, u64>,
    replaced_event_seqs: HashMap<u64, Vec<u64>>,
    current_seqs: Vec<u64>,
}

/// Classifies a raw event log with one canonical surface fold (dsh
/// `analyzeEventLog`; the fold error maps to `INVALID_SURFACE` with the
/// dsh text).
///
/// # Errors
///
/// The `SESSION_QUERY_INVALID_SURFACE` refusal when the fold fails.
fn analyze_event_log(
    session_id: &str,
    events: &[SessionEvent],
) -> Result<EventLogAnalysis, SessionQueryError> {
    let folded = fr_session::surface::fold_surface(events).map_err(|error| {
        SessionQueryError::new(
            SessionQueryErrorCode::InvalidSurface,
            format!("invalid session surface: {error}"),
        )
    })?;
    let current: HashSet<u64> = folded.nodes.iter().map(|seq| seq.get()).collect();
    let mut replaced_by: HashMap<u64, u64> = HashMap::new();
    let mut replaced_event_seqs: HashMap<u64, Vec<u64>> = HashMap::new();
    for replacement in &folded.replacements {
        replaced_event_seqs.insert(
            replacement.seq.get(),
            replacement
                .shadowed_seqs
                .iter()
                .map(|seq| seq.get())
                .collect::<Vec<_>>(),
        );
        for removed_seq in &replacement.shadowed_seqs {
            replaced_by.insert(removed_seq.get(), replacement.seq.get());
        }
    }
    let records = events
        .iter()
        .map(|event| EventRecord {
            session_id: session_id.to_owned(),
            seq: event.seq.get(),
            event_type: event.event_type().to_owned(),
            time: event.time,
            surface: if current.contains(&event.seq.get()) {
                "current"
            } else if replaced_by.contains_key(&event.seq.get()) {
                "shadowed"
            } else {
                "log-only"
            },
        })
        .collect::<Vec<_>>();
    Ok(EventLogAnalysis {
        records,
        replaced_by,
        replaced_event_seqs,
        current_seqs: folded.nodes.iter().map(|seq| seq.get()).collect::<Vec<_>>(),
    })
}

/// Projects the lightweight raw-log records (dsh `eventRecords`).
///
/// # Errors
///
/// The `SESSION_QUERY_INVALID_SURFACE` refusal when the fold fails.
pub fn event_records(
    session_id: &str,
    events: &[SessionEvent],
) -> Result<Vec<EventRecord>, SessionQueryError> {
    Ok(analyze_event_log(session_id, events)?.records)
}

/// Folds and returns the current model surface after validating the
/// whole log (dsh `currentSurfaceEvents`).
///
/// # Errors
/// The `SESSION_QUERY_INVALID_SURFACE` refusal when the fold fails or a
/// current node is not a surface event.
pub fn current_surface_events(
    session_id: &str,
    events: &[SessionEvent],
) -> Result<Vec<SessionEvent>, SessionQueryError> {
    let analysis = analyze_event_log(session_id, events)?;
    analysis
        .current_seqs
        .iter()
        .filter_map(|seq| {
            let event = events.get(usize::try_from(*seq).unwrap_or(usize::MAX));
            event.map(|event| {
                if event.seq.get() != *seq || !fr_session::surface::is_surface_eligible(event) {
                    return Err(SessionQueryError::new(
                        SessionQueryErrorCode::InvalidSurface,
                        format!(
                            "invalid session surface: current node {seq} is not a surface event"
                        ),
                    ));
                }
                Ok(event.clone())
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

/// Traces one target's replacements and relationships (dsh
/// `traceEvent`).
///
/// # Errors
///
/// The absent-target and surface validation refusals.
pub fn trace_event(
    session_id: &str,
    events: &[SessionEvent],
    seq: u64,
) -> Result<EventTrace, SessionQueryError> {
    let Some(target) = events.get(usize::try_from(seq).unwrap_or(usize::MAX)) else {
        return Err(not_found_event(session_id, seq));
    };
    if target.seq.get() != seq {
        return Err(not_found_event(session_id, seq));
    }
    let analysis = analyze_event_log(session_id, events)?;
    let mut replacement_chain: Vec<u64> = Vec::new();
    let mut replacement = analysis.replaced_by.get(&seq).copied();
    while let Some(next) = replacement {
        replacement_chain.push(next);
        replacement = analysis.replaced_by.get(&next).copied();
    }
    let derived_event_seqs: Vec<u64> = events
        .iter()
        .filter(|event| event.seq.get() > seq)
        .filter(|event| {
            event
                .source_event_seqs
                .as_ref()
                .is_some_and(|sources| sources.iter().any(|source| source.get() == seq))
        })
        .map(|event| event.seq.get())
        .collect();
    let Some(target_record) = analysis
        .records
        .get(usize::try_from(seq).unwrap_or(usize::MAX))
    else {
        return Err(not_found_event(session_id, seq));
    };
    let replaced_by = analysis.replaced_by.get(&seq).copied();
    Ok(EventTrace {
        target: target_record.clone(),
        replaced_by,
        replacement_chain,
        replaced_event_seqs: analysis
            .replaced_event_seqs
            .get(&seq)
            .cloned()
            .unwrap_or_default(),
        source_event_seqs: target
            .source_event_seqs
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|seq| seq.get())
            .collect::<Vec<_>>(),
        derived_event_seqs,
    })
}

/// The absent-event refusal (dsh text, verbatim).
fn not_found_event(session_id: &str, seq: u64) -> SessionQueryError {
    SessionQueryError::new(
        SessionQueryErrorCode::EventNotFound,
        format!("session \"{session_id}\" has no event at seq {seq}"),
    )
}

/// One logical session record (dsh `SessionRecord`).
#[derive(Debug, Clone)]
pub struct SessionRecord {
    /// Cloned session header selected from the live-preferred corpus.
    pub header: Value,
    /// Whether the id currently exists in the live store.
    pub live: bool,
    /// Whether the persistence backend currently lists the id.
    pub persisted: bool,
}

/// Compares two corpus records newest-first (dsh `compareSessions`:
/// createdAt desc, then id locale-compare — the byte order is the fr
/// locale-compare equivalent for id strings).
#[must_use]
pub fn compare_sessions(a: &SessionRecord, b: &SessionRecord) -> std::cmp::Ordering {
    let created_a = header_created_at(&a.header).unwrap_or(0.0);
    let created_b = header_created_at(&b.header).unwrap_or(0.0);
    created_b
        .partial_cmp(&created_a)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            let id_a = header_id(&a.header).unwrap_or_default();
            let id_b = header_id(&b.header).unwrap_or_default();
            id_a.cmp(id_b)
        })
}

/// One recursive descendant node (dsh `SessionLineageNode`).
#[derive(Debug, Clone)]
pub struct LineageNode {
    /// Detached logical-corpus record for this descendant.
    pub session: SessionRecord,
    /// Direct children, each carrying its own recursive descendants.
    pub descendants: Vec<Self>,
}

/// Known ancestry and descendants for one logical session (dsh
/// `SessionLineageTrace`).
#[derive(Debug, Clone)]
pub struct LineageTrace {
    /// Detached record for the session that was traced.
    pub target: SessionRecord,
    /// Known parents from the immediate parent outward.
    pub ancestors: Vec<SessionRecord>,
    /// Complete known descendant trees rooted at the target's direct
    /// children.
    pub descendants: Vec<LineageNode>,
    /// Whether the complete parent chain is present in the corpus.
    pub complete: bool,
    /// The root record when the lineage is complete (the top of the
    /// complete lineage).
    pub root: Option<SessionRecord>,
    /// The first parent id that is not present in the corpus, when the
    /// parent chain leaves the visible corpus.
    pub unresolved_parent_id: Option<String>,
}

/// Traces one target's known ancestry and recursively known descendants
/// (dsh `traceSession`: the ancestry walk cycles fail
/// `INVALID_LINEAGE`; children sort createdAt asc then id).
///
/// # Errors
///
/// The absent-target and lineage-cycle refusals.
pub fn trace_session(
    records: &[SessionRecord],
    session_id: &str,
) -> Result<LineageTrace, SessionQueryError> {
    let by_id: HashMap<String, &SessionRecord> = records
        .iter()
        .filter_map(|record| header_id(&record.header).map(|id| (id.to_owned(), record)))
        .collect();
    let Some(target) = by_id.get(session_id).copied() else {
        return Err(SessionQueryError::new(
            SessionQueryErrorCode::SessionNotFound,
            format!("session \"{session_id}\" not found"),
        ));
    };
    let mut ancestors: Vec<SessionRecord> = Vec::new();
    let mut ancestry_seen: HashSet<String> = HashSet::new();
    ancestry_seen.insert(session_id.to_owned());
    let mut unresolved_parent_id: Option<String> = None;
    let mut parent_id = header_parent(&target.header).map(str::to_owned);
    while let Some(current) = parent_id {
        if ancestry_seen.contains(&current) {
            return Err(SessionQueryError::new(
                SessionQueryErrorCode::InvalidLineage,
                format!("session lineage contains a cycle at \"{current}\""),
            ));
        }
        ancestry_seen.insert(current.clone());
        let Some(parent) = by_id.get(&current).copied() else {
            unresolved_parent_id = Some(current);
            break;
        };
        ancestors.push(parent.clone());
        parent_id = header_parent(&parent.header).map(str::to_owned);
    }
    let mut children_by_parent: HashMap<String, Vec<SessionRecord>> = HashMap::new();
    for record in records {
        if let Some(parent) = header_parent(&record.header) {
            children_by_parent
                .entry(parent.to_owned())
                .or_default()
                .push(record.clone());
        }
    }
    for children in children_by_parent.values_mut() {
        children.sort_by(compare_sessions);
    }
    let descendants = build_descendants(&children_by_parent, session_id);
    let root = unresolved_parent_id
        .is_none()
        .then(|| ancestors.last().cloned().unwrap_or_else(|| target.clone()));
    Ok(LineageTrace {
        target: target.clone(),
        ancestors: ancestors.clone(),
        descendants,
        complete: unresolved_parent_id.is_none(),
        root,
        unresolved_parent_id,
    })
}

/// Builds the descendant trees (dsh `buildDescendants`: each child
/// carries its recursively built subtree; children in their sorted
/// order).
#[must_use]
pub fn build_descendants<S: std::hash::BuildHasher>(
    children_by_parent: &HashMap<String, Vec<SessionRecord>, S>,
    session_id: &str,
) -> Vec<LineageNode> {
    fn extend<S: std::hash::BuildHasher>(
        children_by_parent: &HashMap<String, Vec<SessionRecord>, S>,
        parent_id: &str,
    ) -> Vec<LineageNode> {
        children_by_parent
            .get(parent_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|record| LineageNode {
                descendants: extend(children_by_parent, &record_id(&record)),
                session: record,
            })
            .collect()
    }
    extend(children_by_parent, session_id)
}

/// Reads one record header's id (the corpus record accessor).
#[must_use]
pub fn record_id(record: &SessionRecord) -> String {
    header_id(&record.header).unwrap_or_default().to_owned()
}

// ---------------------------------------------------------------------------
// Corpus (dsh corpus.ts)
// ---------------------------------------------------------------------------

/// The borrowed source one batch projection receives (dsh
/// `LogicalSessionSource`).
#[derive(Debug)]
pub struct LogicalSessionSource<'a> {
    /// Header selected with the events; callers clone retained output.
    pub header: &'a Value,
    /// Typed events selected with the header (valid only for the call).
    pub events: &'a [SessionEvent],
}

/// One source-projection result (dsh `LogicalProjectionResult`).
#[derive(Debug, Clone)]
pub enum ProjectionResult<V> {
    /// The projection succeeded.
    Fulfilled {
        /// The requested session id.
        session_id: String,
        /// The projected value.
        value: V,
    },
    /// The failure isolated to this session.
    Rejected {
        /// The requested session id.
        session_id: String,
        /// The original failure.
        reason: SessionQueryError,
    },
}

/// Resolves the live-preferred corpus against the persisted face (dsh
/// `SessionCorpus` over the wire seam).
pub struct SessionCorpus {
    wire: Arc<dyn QueryWire>,
    persisted_read_concurrency: u64,
}

impl std::fmt::Debug for SessionCorpus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCorpus").finish_non_exhaustive()
    }
}

impl SessionCorpus {
    /// Assembles the corpus over the source seam and the concurrency
    /// bound.
    #[must_use]
    pub fn new(wire: Arc<dyn QueryWire>, persisted_read_concurrency: u64) -> Self {
        Self {
            wire,
            persisted_read_concurrency,
        }
    }

    /// Lists the complete logical corpus with live precedence and
    /// cloned headers (dsh `listSessions`; the live overlay argument is
    /// the dsh `ctx.sessions.list()` wire equivalent).
    ///
    /// # Errors
    ///
    /// The listing, conflict, and live-read transport refusals.
    pub fn list_sessions(&self, live: &[String]) -> Result<Vec<SessionRecord>, SessionQueryError> {
        let persisted_rows = self
            .wire
            .persisted_list()
            .map_err(|text| listing_failed(&text))?;
        let mut records: HashMap<String, SessionRecord> = HashMap::new();
        for header in persisted_rows {
            let Some(id) = header_id(&header).map(str::to_owned) else {
                continue;
            };
            records.insert(
                id.clone(),
                SessionRecord {
                    header,
                    live: false,
                    persisted: true,
                },
            );
        }
        for id in live {
            let log = self
                .wire
                .session_read(id)
                .map_err(|text| listing_failed(&text))?;
            let Some(log) = log else {
                continue;
            };
            if let Some(durable) = records.get(id.as_str()) {
                assert_session_headers_compatible(&log.header, &durable.header)?;
            }
            records.insert(
                id.clone(),
                SessionRecord {
                    header: log.header,
                    live: true,
                    persisted: records.contains_key(id.as_str()),
                },
            );
        }
        let mut ordered: Vec<SessionRecord> = records.into_values().collect();
        ordered.sort_by(compare_sessions);
        Ok(ordered)
    }

    /// Loads one logical source, preferring a detached live snapshot
    /// (dsh `SessionCorpus.load`: a known live target never consults
    /// persistence; the mounted persisted face's failure cannot make
    /// current in-memory history unreadable).
    ///
    /// # Errors
    ///
    /// The not-found, conflict, and persistence refusals.
    pub fn load(&self, session_id: &str) -> Result<EngineLog, SessionQueryError> {
        if let Some(log) = self
            .wire
            .session_read(session_id)
            .map_err(|text| transport_failed(&text))?
        {
            return decode_log(log).map_err(|text| transport_failed(&text));
        }
        let listed = self
            .wire
            .persisted_list()
            .map_err(|text| listing_failed(&text))?;
        let listed_header = listed
            .iter()
            .find(|header| header_id(header) == Some(session_id))
            .cloned();
        let Some(listed_header) = listed_header else {
            return Err(not_found_session(session_id));
        };
        let loaded = self
            .wire
            .persisted_cold_read(session_id)
            .map_err(|text| transport_failed(&text))?;
        let Some(loaded) = loaded else {
            return Err(not_found_session(session_id));
        };
        if let Some(live) = self
            .wire
            .session_read(session_id)
            .map_err(|text| transport_failed(&text))?
        {
            return decode_log(live).map_err(|text| transport_failed(&text));
        }
        assert_session_headers_compatible(&loaded.header, &listed_header)?;
        decode_log(loaded).map_err(|text| rejected_cold(session_id, &text))
    }

    /// Projects unique logical sources immediately (dsh
    /// `projectMany`: live ids project from their live snapshot; the
    /// unresolved ids resolve against the persisted listing with the
    /// bounded worker pool; per-id failures stay isolated while the
    /// listing failure rejects the batch).
    ///
    /// # Errors
    ///
    /// The listing refusal (all unresolved ids reject on their own
    /// transport failures).
    #[expect(
        clippy::too_many_lines,
        reason = "the batch mirrors the dsh resolution phases one arm per leg; splitting it would hide the live/persisted isolation correspondence"
    )]
    pub fn project_many<V>(
        &self,
        session_ids: &[String],
        project: impl Fn(LogicalSessionSource<'_>) -> Result<V, SessionQueryError>,
        live_lookup: &dyn Fn(&str) -> Option<QueryLog>,
    ) -> Result<Vec<ProjectionResult<V>>, String> {
        let ids: Vec<String> = {
            let mut seen: HashSet<String> = HashSet::new();
            session_ids
                .iter()
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect()
        };
        let mut resolved: HashMap<String, ProjectionResult<V>> = HashMap::new();
        let mut unresolved: Vec<String> = Vec::new();
        for id in &ids {
            if let Some(live) = live_lookup(id.as_str()) {
                let outcome = decode_source_project(&live, &project);
                match outcome {
                    Ok(value) => resolved.insert(
                        id.clone(),
                        ProjectionResult::Fulfilled {
                            session_id: id.clone(),
                            value,
                        },
                    ),
                    Err(reason) => resolved.insert(
                        id.clone(),
                        ProjectionResult::Rejected {
                            session_id: id.clone(),
                            reason,
                        },
                    ),
                };
            } else {
                unresolved.push(id.clone());
            }
        }
        if unresolved.is_empty() {
            return Ok(ordered_results_owned(&ids, &mut resolved));
        }
        let listed = match self.wire.persisted_list() {
            Ok(rows) => rows,
            Err(text) => {
                for id in unresolved {
                    resolved.insert(
                        id.clone(),
                        ProjectionResult::Rejected {
                            session_id: id.clone(),
                            reason: listing_failed_text(&text),
                        },
                    );
                }
                return Ok(ordered_results_owned(&ids, &mut resolved));
            }
        };
        let persisted_by_id: HashMap<String, Value> = listed
            .iter()
            .filter_map(|header| header_id(header).map(|id| (id.to_owned(), header.clone())))
            .collect();
        for chunk in unresolved.chunks(persisted_chunk(self.persisted_read_concurrency)) {
            for id in chunk {
                let Some(listed_header) = persisted_by_id.get(id.as_str()).cloned() else {
                    if let Some(live) = live_lookup(id.as_str()) {
                        match decode_source_project(&live, &project) {
                            Ok(value) => resolved.insert(
                                id.clone(),
                                ProjectionResult::Fulfilled {
                                    session_id: id.clone(),
                                    value,
                                },
                            ),
                            Err(reason) => resolved.insert(
                                id.clone(),
                                ProjectionResult::Rejected {
                                    session_id: id.clone(),
                                    reason,
                                },
                            ),
                        };
                        continue;
                    }
                    resolved.insert(
                        id.clone(),
                        ProjectionResult::Rejected {
                            session_id: id.clone(),
                            reason: not_found_session(id.as_str()),
                        },
                    );
                    continue;
                };
                let loaded = self.wire.persisted_cold_read(id.as_str());
                let outcome = match loaded {
                    Err(text) => Err(rejected_cold(id.as_str(), &text)),
                    Ok(None) => Err(not_found_session(id.as_str())),
                    Ok(Some(log)) => {
                        if let Some(live) = live_lookup(id.as_str()) {
                            decode_source_project(&live, &project)
                        } else if let Err(conflict) =
                            assert_session_headers_compatible(&log.header, &listed_header)
                        {
                            Err(conflict)
                        } else {
                            decode_source_project(&log, &project)
                        }
                    }
                };
                if let Ok(value) = outcome {
                    resolved.insert(
                        id.clone(),
                        ProjectionResult::Fulfilled {
                            session_id: id.clone(),
                            value,
                        },
                    );
                }
            }
        }
        Ok(ordered_results_owned(&ids, &mut resolved))
    }
}

/// The persisted-read chunk width (at least one id per pass).
fn persisted_chunk(bound: u64) -> usize {
    usize::try_from(bound.max(1)).unwrap_or(usize::MAX)
}

/// Restores the first-occurrence input order over the resolved map
/// (dsh `orderedResults`; the resolved values move out of the map).
#[must_use]
pub fn ordered_results_owned<S: std::hash::BuildHasher, V>(
    ids: &[String],
    resolved: &mut HashMap<String, ProjectionResult<V>, S>,
) -> Vec<ProjectionResult<V>> {
    ids.iter().filter_map(|id| resolved.remove(id)).collect()
}

/// Decodes one wire log and runs the projection (the batch's per-source
/// fold leg; decode failures reject that source).
/// # Errors
///
/// The typed decode refusal (the caller classifies it).
pub fn decode_source_project<V>(
    log: &QueryLog,
    project: &impl Fn(LogicalSessionSource<'_>) -> Result<V, SessionQueryError>,
) -> Result<V, SessionQueryError> {
    let decoded = decode_log(log.clone()).map_err(|text| transport_failed(&text))?;
    project(LogicalSessionSource {
        header: &decoded.header,
        events: &decoded.events,
    })
}

/// The listing transport failure (dsh listPersisted's mapping).
fn listing_failed(text: &str) -> SessionQueryError {
    listing_failed_text(text)
}

/// The listing transport failure text (dsh `session persistence listing
/// failed: ...`).
fn listing_failed_text(text: &str) -> SessionQueryError {
    SessionQueryError::new(
        SessionQueryErrorCode::PersistenceFailed,
        format!("session persistence listing failed: {text}"),
    )
}

/// The generic transport failure (the live/cold decode legs).
fn transport_failed(text: &str) -> SessionQueryError {
    SessionQueryError::new(
        SessionQueryErrorCode::PersistenceFailed,
        format!("failed to read stored session: {text}"),
    )
}

/// The cold-read transport failure (dsh inspectPersisted's mapping:
/// the corruption-name prefix classifies, absence falls through).
fn rejected_cold(session_id: &str, text: &str) -> SessionQueryError {
    if text.starts_with("SessionPersistenceCorruptionError") {
        return SessionQueryError::new(
            SessionQueryErrorCode::CorruptSession,
            format!("stored session \"{session_id}\" is corrupt: {text}"),
        );
    }
    SessionQueryError::new(
        SessionQueryErrorCode::PersistenceFailed,
        format!("failed to read stored session \"{session_id}\": {text}"),
    )
}

/// Builds the transport-class failure from one wire text (the tool
/// bodies' caller-read leg mapping).
#[must_use]
pub fn transport_error(text: &str) -> SessionQueryError {
    SessionQueryError::new(
        SessionQueryErrorCode::PersistenceFailed,
        format!("failed to read stored session: {text}"),
    )
}

/// The absent-session refusal (dsh text, verbatim).
fn not_found_session(session_id: &str) -> SessionQueryError {
    SessionQueryError::new(
        SessionQueryErrorCode::SessionNotFound,
        format!("session \"{session_id}\" not found"),
    )
}

// ---------------------------------------------------------------------------
// Filters (dsh filters.ts)
// ---------------------------------------------------------------------------

/// One logical-session predicate (dsh `SessionResultFilter`).
#[derive(Debug, Clone)]
pub enum SessionFilter {
    /// Id membership.
    Id {
        /// The accepted session ids.
        values: Vec<String>,
    },
    /// Working-directory membership with the null arm.
    Cwd {
        /// The accepted directories; null accepts absent.
        values: Vec<Option<String>>,
    },
    /// Creation-time range.
    CreatedAt {
        /// Inclusive lower bound.
        from: Option<f64>,
        /// Inclusive upper bound.
        to: Option<f64>,
    },
    /// Parent membership with the null arm.
    Parent {
        /// The accepted parent ids; null accepts roots.
        values: Vec<Option<String>>,
    },
    /// Source availability membership.
    Availability {
        /// The accepted availability words.
        values: Vec<&'static str>,
    },
}

/// One event predicate (dsh `SessionEventResultFilter`).
#[derive(Debug, Clone)]
pub enum EventFilter {
    /// Sequence range.
    Seq {
        /// Inclusive lower bound.
        from: Option<f64>,
        /// Inclusive upper bound.
        to: Option<f64>,
    },
    /// Event-time range.
    Time {
        /// Inclusive lower bound.
        from: Option<f64>,
        /// Inclusive upper bound.
        to: Option<f64>,
    },
    /// Type membership.
    Type {
        /// The accepted event types.
        values: Vec<String>,
    },
    /// Surface membership.
    Surface {
        /// The accepted surfaces.
        values: Vec<String>,
    },
    /// Literal case-insensitive whitespace-flexible text scan.
    Text {
        /// The caller-provided literal text.
        text: String,
    },
}

/// Builds the literal case-insensitive, whitespace-flexible text match
/// (dsh `compileSessionTextFilter`; the regex crate's case-insensitive
/// Unicode flag set).
///
/// # Errors
///
/// The `SESSION_QUERY_INVALID_FILTER` refusal on blank text.
/// # Panics
///
/// Panics when the escaped literal pattern stops compiling (a regex
/// crate regression; the escape guarantees the shape).
pub fn compile_session_text_filter(text: &str) -> Result<regex::Regex, SessionQueryError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(SessionQueryError::new(
            SessionQueryErrorCode::InvalidFilter,
            "session text filter must contain non-whitespace text",
        ));
    }
    let pattern = trimmed
        .split_whitespace()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join("\\s+");
    Ok(regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .expect("the escaped literal pattern compiles"))
}

/// Converts one integer record field into the dsh comparison domain
/// (the JS numbers are f64 doubles; the cast IS the dsh semantics, and
/// the safe-integer domain keeps the exactness).
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "the dsh comparison domain is the f64 double; the safe-integer bound preserves exactness"
)]
pub fn to_f64(value: impl Into<u64>) -> f64 {
    value.into() as f64
}

/// The signed field's conversion (the same dsh double domain).
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "the dsh comparison domain is the f64 double; the safe-integer bound preserves exactness"
)]
pub const fn signed_to_f64(value: i64) -> f64 {
    value as f64
}

/// Whether one inclusive range accepts a value (dsh `matchesRange`).
#[must_use]
pub fn matches_range(value: f64, from: Option<f64>, to: Option<f64>) -> bool {
    from.is_none_or(|lower| value >= lower) && to.is_none_or(|upper| value <= upper)
}

/// Applies the `ANDed` session filters in input order (dsh
/// `filterSessionResults`).
#[must_use]
pub fn filter_session_results(
    records: &[SessionRecord],
    filters: &[SessionFilter],
) -> Vec<SessionRecord> {
    records
        .iter()
        .filter(|record| {
            filters.iter().all(|filter| match filter {
                SessionFilter::Id { values } => {
                    header_id(&record.header).is_some_and(|id| values.iter().any(|v| v == id))
                }
                SessionFilter::Cwd { values } => {
                    let record_cwd = header_cwd(&record.header);
                    values.iter().any(|value| value.as_deref() == record_cwd)
                }
                SessionFilter::CreatedAt { from, to } => header_created_at(&record.header)
                    .is_some_and(|created| matches_range(created, *from, *to)),
                SessionFilter::Parent { values } => {
                    let record_parent = header_parent(&record.header);
                    values.iter().any(|value| value.as_deref() == record_parent)
                }
                SessionFilter::Availability { values } => values.iter().any(|value| match *value {
                    "live" => record.live,
                    "persisted" => record.persisted,
                    _ => false,
                }),
            })
        })
        .cloned()
        .collect()
}

/// Applies the `ANDed` event filters over the semantic documents (dsh
/// `filterSessionEventDocuments`).
#[must_use]
pub fn filter_session_event_documents(
    documents: &[SearchDocument],
    filters: &[EventFilter],
) -> Vec<SearchDocument> {
    documents
        .iter()
        .filter(|document| {
            filters.iter().all(|filter| match filter {
                EventFilter::Seq { from, to } => matches_range(to_f64(document.seq), *from, *to),
                EventFilter::Time { from, to } => {
                    matches_range(signed_to_f64(document.time), *from, *to)
                }
                EventFilter::Type { values } => values.contains(&document.event_type),
                EventFilter::Surface { values } => {
                    values.iter().any(|value| value == document.surface)
                }
                EventFilter::Text { text } => compiled_text_probe(text)
                    .is_some_and(|pattern| pattern.is_match(&document.text)),
            })
        })
        .cloned()
        .collect()
}

/// Compiles one literal text filter for the predicate scan (the per-call
/// compile; blank text refuses through the same dsh text).
fn compiled_text_probe(text: &str) -> Option<regex::Regex> {
    compile_session_text_filter(text).ok()
}

// ---------------------------------------------------------------------------
// Documents (dsh documents.ts)
// ---------------------------------------------------------------------------

/// One searchable semantic document (dsh `SessionEventSearchDocument`).
#[derive(Debug, Clone)]
pub struct SearchDocument {
    /// Session that owns the event.
    pub session_id: String,
    /// Monotonic event seq.
    pub seq: u64,
    /// Discriminant of the session event.
    pub event_type: String,
    /// Event timestamp in Unix epoch milliseconds.
    pub time: i64,
    /// Event placement in the folded session surface.
    pub surface: &'static str,
    /// The first-party semantic text.
    pub text: String,
}

/// Projects a raw log into searchable semantic documents (dsh
/// `buildSessionEventSearchDocuments`: the surface classification folds
/// once, structural events omit).
///
/// # Errors
///
/// The `SESSION_QUERY_INVALID_SURFACE` refusal when the fold fails.
pub fn build_session_event_search_documents(
    session_id: &str,
    events: &[SessionEvent],
) -> Result<Vec<SearchDocument>, SessionQueryError> {
    let analysis = analyze_event_log(session_id, events)?;
    let documents: Vec<SearchDocument> = events
        .iter()
        .filter_map(|event| {
            let text = extract_session_event_text(event);
            if text.is_empty() {
                return None;
            }
            let record = analysis
                .records
                .get(usize::try_from(event.seq.get()).unwrap_or(usize::MAX))?;
            Some(SearchDocument {
                session_id: session_id.to_owned(),
                seq: event.seq.get(),
                event_type: record.event_type.clone(),
                time: event.time,
                surface: record.surface,
                text,
            })
        })
        .collect();
    Ok(documents)
}

// ---------------------------------------------------------------------------
// Engine (dsh index.ts SessionQueryEngine)
// ---------------------------------------------------------------------------

/// One exact observation of a session (dsh `SessionObservation`; the
/// wire-backed cut: the header, inherited count, balanced events, and
/// last observed seq).
#[derive(Debug, Clone)]
pub struct Observation {
    /// Whether the cut came from the live store or a stored log.
    pub source: &'static str,
    /// Immutable session identity metadata.
    pub header: Value,
    /// Exact fork-inherited event count paired with the header.
    pub inherited_event_count: u64,
    /// Immutable contiguous events at the cut.
    pub events: Vec<SessionEvent>,
    /// Last observed event seq, or -1 for an empty log (the dsh
    /// `cursor`).
    pub cursor: i64,
}

/// The engine (dsh `SessionQueryEngine`): the backend-independent
/// concrete reads over the corpus, plus the search-disabled terminal
/// state.
pub struct SessionQueryEngine {
    corpus: SessionCorpus,
    read_window_max: u64,
}

impl std::fmt::Debug for SessionQueryEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionQueryEngine").finish_non_exhaustive()
    }
}

impl SessionQueryEngine {
    /// The source seam (the tool bodies' projection reads).
    #[must_use]
    pub fn wire(&self) -> &Arc<dyn QueryWire> {
        &self.corpus.wire
    }

    /// Assembles the engine over the source seam and the resolved
    /// config.
    #[must_use]
    pub fn new(wire: Arc<dyn QueryWire>, config: EngineConfig) -> Self {
        Self {
            corpus: SessionCorpus::new(wire, config.persisted_read_concurrency),
            read_window_max: config.read_window_max,
        }
    }

    /// Observes one exact live or stored session without a listing
    /// preflight (dsh `observeSession`): the live cut wins; the stored
    /// read maps its failures through the query taxonomy.
    ///
    /// # Errors
    ///
    /// The observation taxonomy family.
    pub fn observe(&self, session_id: &str) -> Result<Observation, SessionQueryError> {
        if let Some(log) = self
            .corpus
            .wire
            .session_read(session_id)
            .map_err(|text| transport_failed(&text))?
        {
            let decoded = decode_log(log).map_err(|text| transport_failed(&text))?;
            return Ok(live_observation(&decoded));
        }
        let loaded = self
            .corpus
            .wire
            .persisted_cold_read(session_id)
            .map_err(|text| rejected_cold(session_id, &text))?;
        let Some(loaded) = loaded else {
            return Err(not_found_session(session_id));
        };
        if let Some(live) = self
            .corpus
            .wire
            .session_read(session_id)
            .map_err(|text| transport_failed(&text))?
        {
            let decoded = decode_log(live).map_err(|text| transport_failed(&text))?;
            return Ok(live_observation(&decoded));
        }
        let decoded = decode_log(loaded).map_err(|text| rejected_cold(session_id, &text))?;
        Ok(Observation {
            source: "prepared",
            header: decoded.header,
            inherited_event_count: decoded.inherited_event_count,
            events: decoded.events.clone(),
            cursor: last_cursor(&decoded.events),
        })
    }

    /// Lists the corpus (the corpus list over the live overlay).
    ///
    /// # Errors
    ///
    /// The corpus listing refusals.
    pub fn list_sessions(&self, live: &[String]) -> Result<Vec<SessionRecord>, SessionQueryError> {
        self.corpus.list_sessions(live)
    }

    /// Reads and replay-validates one complete log without making it
    /// live (dsh `readSession`; the Session.create replay equivalent is
    /// the typed decode — the contiguous-seq validation rides the wire
    /// faces' own checks).
    ///
    /// # Errors
    ///
    /// The corpus load refusals.
    pub fn read_session(&self, session_id: &str) -> Result<EngineLog, SessionQueryError> {
        self.corpus.load(session_id)
    }

    /// Filters the corpus with the typed predicates (dsh
    /// `filterSessions`).
    ///
    /// # Errors
    ///
    /// The corpus listing refusals.
    pub fn filter_sessions(
        &self,
        filters: &[SessionFilter],
        live: &[String],
    ) -> Result<Vec<SessionRecord>, SessionQueryError> {
        let records = self.corpus.list_sessions(live)?;
        Ok(filter_session_results(&records, filters))
    }

    /// Folds the latest log-backed title from one session (the corpus
    /// load + the last `session/title` snapshot read; the fr title fold
    /// reads the title snapshot the same way the goal projection reads
    /// its fold state — the dsh `foldSessionTitle` shape).
    ///
    /// # Errors
    ///
    /// The corpus load refusals.
    pub fn read_title(&self, session_id: &str) -> Result<Option<Value>, SessionQueryError> {
        Ok(self.read_title_snapshot(session_id)?.title)
    }

    /// Folds the latest title and returns its source header (dsh
    /// `readTitleSnapshot`).
    ///
    /// # Errors
    ///
    /// The corpus load refusals.
    pub fn read_title_snapshot(
        &self,
        session_id: &str,
    ) -> Result<TitleObservation, SessionQueryError> {
        let loaded = self.corpus.load(session_id)?;
        Ok(TitleObservation {
            session: loaded.header,
            title: last_title_snapshot(&loaded.events),
        })
    }

    /// Folds titles for unique sessions (dsh `readTitleSnapshots`:
    /// first-occurrence order, per-session isolation).
    ///
    /// # Errors
    ///
    /// The listing refusal rides the batch (per-id failures stay
    /// isolated).
    pub fn read_title_snapshots(
        &self,
        session_ids: &[String],
        live_lookup: &dyn Fn(&str) -> Option<QueryLog>,
    ) -> Vec<ProjectionResult<TitleObservation>> {
        self.corpus
            .project_many(
                session_ids,
                |source| {
                    Ok(TitleObservation {
                        session: source.header.clone(),
                        title: last_title_snapshot(source.events),
                    })
                },
                live_lookup,
            )
            .unwrap_or_else(|reason| {
                session_ids
                    .iter()
                    .map(|id| ProjectionResult::Rejected {
                        session_id: id.clone(),
                        reason: listing_failed_text(&reason),
                    })
                    .collect()
            })
    }

    /// Lists lightweight raw-log event records for one session (dsh
    /// `listEvents`).
    ///
    /// # Errors
    ///
    /// The corpus load and surface refusals.
    pub fn list_events(&self, session_id: &str) -> Result<Vec<EventRecord>, SessionQueryError> {
        let loaded = self.corpus.load(session_id)?;
        event_records(session_id, &loaded.events)
    }

    /// Scans the semantic documents with provider-independent filters
    /// (dsh `filterEvents`).
    ///
    /// # Errors
    ///
    /// The corpus load and surface refusals.
    pub fn filter_events(
        &self,
        session_id: &str,
        filters: &[EventFilter],
    ) -> Result<Vec<SearchDocument>, SessionQueryError> {
        let loaded = self.corpus.load(session_id)?;
        let documents = build_session_event_search_documents(session_id, &loaded.events)?;
        Ok(filter_session_event_documents(&documents, filters))
    }

    /// Reads one session's complete current model surface (dsh
    /// `readSurface`).
    ///
    /// # Errors
    ///
    /// The corpus load and surface refusals.
    pub fn read_surface(&self, session_id: &str) -> Result<SurfaceSnapshot, SessionQueryError> {
        let loaded = self.corpus.load(session_id)?;
        let events = current_surface_events(session_id, &loaded.events)?;
        Ok(SurfaceSnapshot {
            session: loaded.header,
            inherited_event_count: loaded.inherited_event_count,
            captured_through_seq: loaded.events.last().map(|event| event.seq.get()),
            events,
        })
    }

    /// Traces known ancestry and descendants (dsh `traceSession`).
    ///
    /// # Errors
    ///
    /// The corpus listing, absent-target, and lineage-cycle refusals.
    pub fn trace_session(
        &self,
        session_id: &str,
        live: &[String],
    ) -> Result<LineageTrace, SessionQueryError> {
        let records = self.corpus.list_sessions(live)?;
        trace_session(&records, session_id)
    }

    /// Traces one event's replacements and cited sources (dsh
    /// `traceEvent`).
    ///
    /// # Errors
    ///
    /// The corpus load, absent-target, and surface refusals.
    pub fn trace_event(
        &self,
        request_session_id: &str,
        seq: u64,
    ) -> Result<EventTraceObservation, SessionQueryError> {
        let loaded = self.corpus.load(request_session_id)?;
        let trace = trace_event(request_session_id, &loaded.events, seq)?;
        Ok(EventTraceObservation {
            session: loaded.header,
            trace,
        })
    }

    /// Reads one full event plus a bounded raw-log window (dsh
    /// `readEvent`).
    ///
    /// # Errors
    ///
    /// The window, corpus, and absent-event refusals.
    pub fn read_event(
        &self,
        session_id: &str,
        seq: u64,
        before: Option<u64>,
        after: Option<u64>,
    ) -> Result<EventWindow, SessionQueryError> {
        let before = self.read_window("before", before)?;
        let after = self.read_window("after", after)?;
        let loaded = self.corpus.load(session_id)?;
        let Some(target) = loaded
            .events
            .get(usize::try_from(seq).unwrap_or(usize::MAX))
        else {
            return Err(not_found_event(session_id, seq));
        };
        if target.seq.get() != seq {
            return Err(not_found_event(session_id, seq));
        }
        let start_seq = seq.saturating_sub(before);
        let end_seq = (seq + after).min(u64::try_from(loaded.events.len() - 1).unwrap_or(0));
        let events = loaded.events[usize::try_from(start_seq).unwrap_or(usize::MAX)
            ..=usize::try_from(end_seq).unwrap_or(usize::MAX)]
            .to_vec();
        Ok(EventWindow {
            session: loaded.header,
            inherited_event_count: loaded.inherited_event_count,
            target: loaded.events[usize::try_from(seq).unwrap_or(usize::MAX)].clone(),
            events,
            start_seq,
            end_seq,
        })
    }

    /// Validates one read-window bound (dsh `_readWindow`; the dsh
    /// text, verbatim).
    ///
    /// # Errors
    ///
    /// The `SESSION_QUERY_INVALID_WINDOW` refusal.
    fn read_window(&self, name: &str, value: Option<u64>) -> Result<u64, SessionQueryError> {
        let Some(value) = value else {
            return Ok(0);
        };
        if value > self.read_window_max {
            return Err(SessionQueryError::new(
                SessionQueryErrorCode::InvalidWindow,
                format!(
                    "{name} must be an integer between 0 and {}",
                    self.read_window_max
                ),
            ));
        }
        Ok(value)
    }

    /// Searches the corpus grouped by session (dsh `searchSessions`):
    /// the M1 terminal state refuses with the disabled code.
    ///
    /// # Errors
    ///
    /// The `SESSION_QUERY_SEARCH_DISABLED` refusal.
    pub fn search_sessions(&self) -> Result<SessionSearchPage, SessionQueryError> {
        Err(search_disabled())
    }

    /// Searches events within one session (dsh `searchEvents`): the M1
    /// terminal state refuses with the disabled code.
    ///
    /// # Errors
    ///
    /// The `SESSION_QUERY_SEARCH_DISABLED` refusal.
    pub fn search_events(&self) -> Result<SessionEventSearchPage, SessionQueryError> {
        Err(search_disabled())
    }
}

/// The disabled-search refusal (the Q16d-2A terminal state).
#[must_use]
pub fn search_disabled() -> SessionQueryError {
    SessionQueryError::new(
        SessionQueryErrorCode::SearchDisabled,
        "session search is disabled in this deployment",
    )
}

/// The last observed seq as the dsh cursor (-1 for empty).
#[must_use]
pub fn last_cursor(events: &[SessionEvent]) -> i64 {
    events
        .last()
        .map_or(-1, |event| i64::try_from(event.seq.get()).unwrap_or(-1))
}

/// Builds the live observation cut.
#[must_use]
pub fn live_observation(decoded: &EngineLog) -> Observation {
    Observation {
        source: "live",
        header: decoded.header.clone(),
        inherited_event_count: decoded.inherited_event_count,
        events: decoded.events.clone(),
        cursor: last_cursor(&decoded.events),
    }
}

/// One folded title bound to its source header (dsh
/// `SessionTitleObservation`).
#[derive(Debug, Clone)]
pub struct TitleObservation {
    /// The cloned source header.
    pub session: Value,
    /// The latest title snapshot, absent when the log has none.
    pub title: Option<Value>,
}

/// One surface snapshot (dsh `SessionSurfaceSnapshot`).
#[derive(Debug)]
pub struct SurfaceSnapshot {
    /// The cloned header.
    pub session: Value,
    /// The exact inherited prefix length.
    pub inherited_event_count: u64,
    /// The highest raw-log seq included, absent for an empty log.
    pub captured_through_seq: Option<u64>,
    /// The current surface events in model-history order.
    pub events: Vec<SessionEvent>,
}

/// One event trace bound to its source header (dsh
/// `SessionEventTraceObservation`).
#[derive(Debug, Clone)]
pub struct EventTraceObservation {
    /// The cloned header selected with the trace's event log.
    pub session: Value,
    /// The trace facts.
    pub trace: EventTrace,
}

/// One event window (dsh `SessionEventWindow`).
#[derive(Debug)]
pub struct EventWindow {
    /// The cloned header for the source read.
    pub session: Value,
    /// The exact inherited prefix length.
    pub inherited_event_count: u64,
    /// The full cloned target event.
    pub target: SessionEvent,
    /// The full cloned events from `start_seq` through `end_seq`.
    pub events: Vec<SessionEvent>,
    /// First seq included.
    pub start_seq: u64,
    /// Last seq included.
    pub end_seq: u64,
}

/// The dsh `SessionSearchPage` marker (the search-disabled terminal
/// state answers no pages; the type pins the API shape).
#[derive(Debug, Clone)]
pub struct SessionSearchPage {
    /// Empty: the terminal state never produces hits.
    pub items: Vec<()>,
}

/// The dsh `SessionEventSearchPage` marker (the search-disabled
/// terminal state answers no pages).
#[derive(Debug, Clone)]
pub struct SessionEventSearchPage {
    /// Empty: the terminal state never produces hits.
    pub items: Vec<()>,
}

/// Folds the latest `session/title` snapshot from the typed log (the dsh
/// `foldSessionTitle` equivalent over the unknown-type retention).
#[must_use]
pub fn last_title_snapshot(events: &[SessionEvent]) -> Option<Value> {
    let mut latest: Option<Value> = None;
    for event in events {
        if let SessionEventData::Unknown(unknown) = &event.data {
            if unknown.type_name != "session/title" {
                continue;
            }
            let mut snapshot = Value::Object(unknown.data.clone());
            if let Some(object) = snapshot.as_object_mut() {
                object.insert("eventSeq".to_owned(), serde_json::json!(event.seq.get()));
                object.insert("updatedAt".to_owned(), serde_json::json!(event.time));
            }
            latest = Some(snapshot);
        }
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The scripted wire: fixed logs per id, one persisted header row,
    /// and a projection read the fixture programs.
    #[derive(Default)]
    struct ScriptedWire {
        live: std::sync::Mutex<HashMap<String, QueryLog>>,
        persisted: std::sync::Mutex<HashMap<String, QueryLog>>,
        persisted_error: std::sync::Mutex<Option<String>>,
        boundary: Option<Value>,
    }

    impl ScriptedWire {
        fn with_live(id: &str, events: Vec<Value>) -> Self {
            let wire = Self::default();
            wire.live.lock().expect("lock").insert(
                id.to_owned(),
                QueryLog {
                    header: header_for(id, None),
                    inherited_event_count: 0,
                    events,
                },
            );
            wire
        }

        fn with_persisted(id: &str, events: Vec<Value>, parent: Option<&str>) -> Self {
            let wire = Self::default();
            wire.persisted.lock().expect("lock").insert(
                id.to_owned(),
                QueryLog {
                    header: header_for(id, parent),
                    inherited_event_count: 0,
                    events,
                },
            );
            wire
        }

        fn persisted_headers(&self) -> Vec<Value> {
            self.persisted
                .lock()
                .expect("lock")
                .values()
                .map(|log| log.header.clone())
                .collect()
        }
    }

    /// One well-formed header row (the fixture's builder).
    fn header_for(id: &str, parent: Option<&str>) -> Value {
        let mut header = json!({
            "id": id,
            "createdAt": 1_000,
            "isSeeded": false,
        });
        if let Some(parent) = parent {
            header["parentSession"] = json!(parent);
        }
        header
    }

    /// One text user message (the fixture event builder).
    fn user_event(seq: u64, text: &str) -> Value {
        json!({
            "seq": seq,
            "time": 1_000 + seq,
            "type": "user/message",
            "surfaceOp": "append",
            "data": {
                "role": "user",
                "id": format!("usr-{seq}"),
                "content": [{ "type": "text", "text": text }],
                "source": { "kind": "user" },
            },
        })
    }

    impl QueryWire for ScriptedWire {
        fn session_read(&self, id: &str) -> Result<Option<QueryLog>, String> {
            Ok(self.live.lock().expect("lock").get(id).cloned())
        }

        fn persisted_list(&self) -> Result<Vec<Value>, String> {
            let error = self.persisted_error.lock().expect("lock").clone();
            if let Some(text) = error {
                return Err(text);
            }
            Ok(self.persisted_headers())
        }

        fn persisted_cold_read(&self, id: &str) -> Result<Option<QueryLog>, String> {
            Ok(self.persisted.lock().expect("lock").get(id).cloned())
        }

        fn projection_state(&self, _session: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(self.boundary.clone())
        }
    }

    fn engine(wire: ScriptedWire) -> SessionQueryEngine {
        SessionQueryEngine::new(Arc::new(wire), EngineConfig::default())
    }

    #[test]
    fn config_resolves_defaults_and_refuses_the_bounds() {
        let resolved = EngineConfig::resolve(None, None, None).expect("defaults");
        assert_eq!(resolved.read_window_max, READ_WINDOW_MAX);
        assert_eq!(resolved.persisted_read_concurrency, 4);
        assert_eq!(resolved.prepared_session_cache_size, 5);
        let zero = EngineConfig::resolve(Some(0), None, None).expect("zero window");
        assert_eq!(zero.read_window_max, 0);
        let error = EngineConfig::resolve(None, Some(0), None).expect_err("zero concurrency");
        assert_eq!(
            error.message,
            "session-query: persistedReadConcurrency must be a positive safe integer"
        );
        let error = EngineConfig::resolve(None, None, Some(0)).expect_err("zero cache");
        assert_eq!(
            error.message,
            "session-query: preparedSessionCacheSize must be a positive safe integer"
        );
    }

    #[test]
    fn headers_compatible_checks_five_fields_and_conflicts_verbatim() {
        let base = header_for("s1", None);
        assert_session_headers_compatible(&base, &base).expect("compatible");
        let mut drifted = base.clone();
        drifted["cwd"] = json!("/elsewhere");
        let error = assert_session_headers_compatible(&base, &drifted).expect_err("cwd conflict");
        assert_eq!(error.code, SessionQueryErrorCode::SourceConflict);
        assert_eq!(
            error.message,
            "session source headers conflict for session \"s1\""
        );
    }

    #[test]
    fn extraction_covers_the_semantic_branches() {
        let user =
            serde_json::from_value::<SessionEvent>(user_event(0, "fix the lock")).expect("user");
        assert_eq!(
            extract_session_event_text(&user),
            "fix the lock",
            "the text block rides alone"
        );
        let call = serde_json::from_value::<SessionEvent>(json!({
            "seq": 1, "time": 2, "type": "tool/call",
            "data": { "turn": 1, "step": 1, "callId": "c1",
                      "name": "bash", "arguments": "{\"command\":\"ls\"}" },
        }))
        .expect("call");
        assert_eq!(
            extract_session_event_text(&call),
            "bash\n{\"command\":\"ls\"}"
        );
        let result = serde_json::from_value::<SessionEvent>(json!({
            "seq": 2, "time": 3, "type": "tool/result",
            "data": { "turn": 1, "step": 1, "callId": "c1",
                      "message": { "role": "tool", "id": "t1", "content": [], "source": { "kind": "tool" } },
                      "error": { "name": "ToolOutcomeUnknownError", "code": "TOOL_OUTCOME_UNKNOWN" } },
        }))
        .expect("result");
        assert_eq!(
            extract_session_event_text(&result),
            "ToolOutcomeUnknownError\nTOOL_OUTCOME_UNKNOWN",
            "the failure identity carries the empty content"
        );
        let todo = serde_json::from_value::<SessionEvent>(json!({
            "seq": 3, "time": 4, "type": "todo/write",
            "data": { "todos": [
                { "status": "completed", "content": "open the door" },
                { "status": "pending", "content": "close the door" },
            ] },
        }))
        .expect("todo");
        assert_eq!(
            extract_session_event_text(&todo),
            "completed\nopen the door\npending\nclose the door"
        );
        let turn_end = serde_json::from_value::<SessionEvent>(json!({
            "seq": 4, "time": 5, "type": "turn/end",
            "data": { "turn": 1, "reason": { "kind": "error",
                      "error": { "message": "route down", "code": "PROVIDER_DOWN" } } },
        }))
        .expect("turn end");
        assert_eq!(extract_session_event_text(&turn_end), "error\nroute down");
        let aborted = serde_json::from_value::<SessionEvent>(json!({
            "seq": 5, "time": 6, "type": "turn/end",
            "data": { "turn": 2, "reason": { "kind": "aborted", "reason": { "kind": "user" } } },
        }))
        .expect("aborted");
        assert_eq!(extract_session_event_text(&aborted), "aborted");
    }

    #[test]
    fn trace_event_walks_the_replacement_and_source_chains() {
        // seq 0 user (surface), seq 1 user (replaces 0), seq 2 cites 0,
        // seq 3 cites 1 (derived of 1), seq 4 log-only.
        let events = vec![
            user_event(0, "first"),
            json!({
                "seq": 1, "time": 2, "type": "user/message",
                "surfaceOp": { "op": "replace", "startSeq": 0, "endSeq": 0 },
                "sourceEventSeqs": [0],
                "data": { "role": "user", "id": "usr-1",
                          "content": [{ "type": "text", "text": "second" }],
                          "source": { "kind": "user" } },
            }),
            json!({
                "seq": 2, "time": 3, "type": "tool/result",
                "surfaceOp": "append",
                "sourceEventSeqs": [0],
                "data": { "turn": 1, "step": 1,
                          "message": { "role": "tool", "id": "t2",
                                       "content": [], "source": { "kind": "tool" } } },
            }),
            json!({
                "seq": 3, "time": 4, "type": "tool/result",
                "surfaceOp": "append",
                "sourceEventSeqs": [1],
                "data": { "turn": 1, "step": 1,
                          "message": { "role": "tool", "id": "t3",
                                       "content": [], "source": { "kind": "tool" } } },
            }),
            json!({
                "seq": 4, "time": 5, "type": "turn/start",
                "data": { "turn": 1 },
            }),
        ];
        let analysis_events = serde_json::from_value::<Vec<SessionEvent>>(Value::Array(events))
            .expect("events decode");
        let trace = trace_event("s1", &analysis_events, 0).expect("trace");
        assert_eq!(trace.replaced_by, Some(1));
        assert_eq!(trace.replacement_chain, vec![1]);
        assert_eq!(trace.replaced_event_seqs, Vec::<u64>::new());
        assert_eq!(trace.source_event_seqs, Vec::<u64>::new());
        // The replace event cites the shadowed node by the admission
        // rule, so it counts as derived alongside the direct citer.
        assert_eq!(trace.derived_event_seqs, vec![1, 2]);
        assert_eq!(trace.target.surface, "shadowed");
        let current = trace_event("s1", &analysis_events, 1).expect("current trace");
        assert_eq!(current.replaced_by, None);
        assert_eq!(current.target.surface, "current");
        assert_eq!(current.derived_event_seqs, vec![3]);
        let absent = trace_event("s1", &analysis_events, 9).expect_err("absent");
        assert_eq!(absent.code, SessionQueryErrorCode::EventNotFound);
        assert_eq!(absent.message, "session \"s1\" has no event at seq 9");
    }

    #[test]
    fn corpus_lists_live_over_persisted_and_refuses_conflicts() {
        let persisted = ScriptedWire::with_persisted("s1", vec![user_event(0, "stored")], None);
        persisted
            .persisted
            .lock()
            .expect("lock")
            .get_mut("s1")
            .expect("row")
            .header["createdAt"] = json!(5_000);
        let headers = persisted.persisted_headers();
        let live = ScriptedWire::with_live("s2", vec![user_event(0, "current")]);
        for header in headers {
            live.persisted.lock().expect("lock").insert(
                "s1".to_owned(),
                QueryLog {
                    header,
                    inherited_event_count: 0,
                    events: vec![user_event(0, "stored")],
                },
            );
        }
        let listing = engine(live);
        let records = listing.list_sessions(&["s2".to_owned()]).expect("listing");
        assert_eq!(records.len(), 2);
        // Newest first: the live session at 1000 sorts after 5000.
        assert_eq!(super::record_id(&records[0]), "s1");
        assert!(!records[0].live && records[0].persisted);
        assert_eq!(super::record_id(&records[1]), "s2");
        assert!(records[1].live && !records[1].persisted);

        // A conflicting header observation refuses the listing: the
        // stored row's createdAt differs from the live header.
        let conflicting = ScriptedWire::with_live("s1", vec![user_event(0, "drifted")]);
        conflicting.persisted.lock().expect("lock").insert(
            "s1".to_owned(),
            QueryLog {
                header: json!({
                    "id": "s1", "createdAt": 5_000, "isSeeded": false,
                }),
                inherited_event_count: 0,
                events: Vec::new(),
            },
        );
        let engine = engine(conflicting);
        let error = engine
            .list_sessions(&["s1".to_owned()])
            .expect_err("conflict");
        assert_eq!(error.code, SessionQueryErrorCode::SourceConflict);
    }

    #[test]
    fn load_prefers_live_and_answers_persisted_then_absent() {
        let persisted = ScriptedWire::with_persisted(
            "s1",
            vec![user_event(0, "stored"), user_event(1, "more")],
            None,
        );
        let cold_engine = engine(persisted);
        let loaded = cold_engine.read_session("s1").expect("cold load");
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.header["id"], "s1");
        let error = cold_engine.read_session("gone").expect_err("absent");
        assert_eq!(error.code, SessionQueryErrorCode::SessionNotFound);
        assert_eq!(error.message, "session \"gone\" not found");

        let live = ScriptedWire::with_live("s1", vec![user_event(0, "current")]);
        let live_engine = engine(live);
        let loaded = live_engine.read_session("s1").expect("live load");
        assert_eq!(
            crate::session_query::extract_session_event_text(&loaded.events[0]),
            "current",
            "the live snapshot wins"
        );
    }

    #[test]
    fn trace_session_walks_ancestors_and_refuses_cycles() {
        let wire = ScriptedWire::default();
        {
            let mut persisted = wire.persisted.lock().expect("lock");
            persisted.insert(
                "root".to_owned(),
                QueryLog {
                    header: header_for("root", None),
                    inherited_event_count: 0,
                    events: Vec::new(),
                },
            );
            persisted.insert(
                "child".to_owned(),
                QueryLog {
                    header: header_for("child", Some("root")),
                    inherited_event_count: 0,
                    events: Vec::new(),
                },
            );
        }
        let child_engine = engine(wire);
        let trace = child_engine.trace_session("child", &[]).expect("lineage");
        assert!(trace.complete);
        assert_eq!(
            trace
                .ancestors
                .iter()
                .map(super::record_id)
                .collect::<Vec<_>>(),
            vec!["root"]
        );
        assert_eq!(
            trace.root.as_ref().map(super::record_id),
            Some("root".to_owned())
        );

        // An unresolvable parent leaves the corpus incomplete.
        let wire = ScriptedWire::default();
        wire.persisted.lock().expect("lock").insert(
            "orphan".to_owned(),
            QueryLog {
                header: header_for("orphan", Some("unknown-parent")),
                inherited_event_count: 0,
                events: Vec::new(),
            },
        );
        let orphan_engine = engine(wire);
        let trace = orphan_engine.trace_session("orphan", &[]).expect("orphan");
        assert!(!trace.complete);
        assert_eq!(
            trace.unresolved_parent_id,
            Some("unknown-parent".to_owned())
        );

        // A cycling parent chain refuses with the dsh text.
        let wire = ScriptedWire::default();
        {
            let mut persisted = wire.persisted.lock().expect("lock");
            persisted.insert(
                "a".to_owned(),
                QueryLog {
                    header: header_for("a", Some("b")),
                    inherited_event_count: 0,
                    events: Vec::new(),
                },
            );
            persisted.insert(
                "b".to_owned(),
                QueryLog {
                    header: header_for("b", Some("a")),
                    inherited_event_count: 0,
                    events: Vec::new(),
                },
            );
        }
        let cycle_engine = engine(wire);
        let error = cycle_engine.trace_session("a", &[]).expect_err("cycle");
        assert_eq!(error.code, SessionQueryErrorCode::InvalidLineage);
        assert_eq!(error.message, "session lineage contains a cycle at \"a\"");
    }

    #[test]
    fn read_event_windows_and_validates_the_bound() {
        let events = (0..5)
            .map(|seq| user_event(seq, &format!("e{seq}")))
            .collect::<Vec<_>>();
        let engine = engine(ScriptedWire::with_live("s1", events));
        let window = engine
            .read_event("s1", 2, Some(2), Some(1))
            .expect("window");
        assert_eq!(window.start_seq, 0);
        assert_eq!(window.end_seq, 3);
        assert_eq!(window.events.len(), 4);
        assert_eq!(window.target.seq.get(), 2);
        let window = engine.read_event("s1", 0, None, None).expect("no window");
        assert_eq!(window.start_seq, 0);
        assert_eq!(window.end_seq, 0);
        let error = engine
            .read_event("s1", 2, Some(READ_WINDOW_MAX + 1), None)
            .expect_err("over window");
        assert_eq!(error.code, SessionQueryErrorCode::InvalidWindow);
        assert_eq!(
            error.message,
            format!("before must be an integer between 0 and {READ_WINDOW_MAX}")
        );
        let error = engine
            .read_event("s1", 99, None, None)
            .expect_err("absent seq");
        assert_eq!(error.message, "session \"s1\" has no event at seq 99");
    }

    #[test]
    fn observe_answers_live_prepared_and_absent() {
        let live_engine = engine(ScriptedWire::with_live("s1", vec![user_event(0, "hi")]));
        let observation = live_engine.observe("s1").expect("live observe");
        assert_eq!(observation.source, "live");
        assert_eq!(observation.cursor, 0);
        let error = live_engine.observe("gone").expect_err("absent");
        assert_eq!(error.code, SessionQueryErrorCode::SessionNotFound);

        let prepared_engine = engine(ScriptedWire::with_persisted(
            "s1",
            vec![user_event(0, "stored")],
            None,
        ));
        let observation = prepared_engine.observe("s1").expect("prepared observe");
        assert_eq!(observation.source, "prepared");
    }

    #[test]
    fn search_answers_the_disabled_terminal_state() {
        let engine = engine(ScriptedWire::default());
        let error = engine.search_sessions().expect_err("sessions search");
        assert_eq!(error.code, SessionQueryErrorCode::SearchDisabled);
        assert_eq!(
            error.message,
            "session search is disabled in this deployment"
        );
        let error = engine.search_events().expect_err("events search");
        assert_eq!(error.code, SessionQueryErrorCode::SearchDisabled);
    }

    #[test]
    fn filters_apply_the_predicates_in_order() {
        let wire = ScriptedWire::default();
        {
            let mut persisted = wire.persisted.lock().expect("lock");
            let mut root_header = header_for("root", None);
            root_header["cwd"] = json!("/repo");
            persisted.insert(
                "root".to_owned(),
                QueryLog {
                    header: root_header,
                    inherited_event_count: 0,
                    events: Vec::new(),
                },
            );
            let mut child_header = header_for("child", Some("root"));
            child_header["cwd"] = json!("/repo");
            persisted.insert(
                "child".to_owned(),
                QueryLog {
                    header: child_header,
                    inherited_event_count: 0,
                    events: Vec::new(),
                },
            );
        }
        let engine = engine(wire);
        let records = engine
            .filter_sessions(
                &[
                    SessionFilter::Cwd {
                        values: vec![Some("/repo".to_owned())],
                    },
                    SessionFilter::Parent {
                        values: vec![Some("root".to_owned()), None],
                    },
                ],
                &[],
            )
            .expect("filtered");
        assert_eq!(
            records.iter().map(super::record_id).collect::<Vec<_>>(),
            vec!["child", "root"],
            "the parent clause ORs null with the listed id, the order stays newest-first"
        );
        let none = engine
            .filter_sessions(
                &[SessionFilter::Availability {
                    values: vec!["live"],
                }],
                &[],
            )
            .expect("availability");
        assert!(none.is_empty());
    }

    #[test]
    fn titles_fold_the_last_snapshot_and_the_text_filter_compiles() {
        let events = vec![
            user_event(0, "first prompt"),
            json!({
                "seq": 1, "time": 2, "type": "session/title",
                "data": { "title": "First title", "messageSeqs": [0],
                          "source": { "kind": "fallback" } },
            }),
            json!({
                "seq": 2, "time": 3, "type": "session/title",
                "data": { "title": "Second title", "messageSeqs": [0],
                          "source": { "kind": "user" } },
            }),
        ];
        let titled_engine = engine(ScriptedWire::with_live("s1", events));
        let title = titled_engine.read_title("s1").expect("title");
        assert_eq!(
            title.expect("snapshot").get("title"),
            Some(&json!("Second title")),
            "the latest snapshot wins"
        );
        let untitled_engine = engine(ScriptedWire::with_live(
            "s1",
            vec![user_event(0, "untitled log")],
        ));
        assert!(untitled_engine.read_title("s1").expect("absent").is_none());

        let pattern = compile_session_text_filter("fix  THE\tlock").expect("pattern");
        assert!(
            pattern.is_match("Fix the LOCK"),
            "the literal scan is case-insensitive and whitespace-flexible"
        );
        let error = compile_session_text_filter("   ").expect_err("blank");
        assert_eq!(
            error.message,
            "session text filter must contain non-whitespace text"
        );
    }

    #[test]
    fn project_many_isolates_failures_and_preserves_order() {
        let wire = ScriptedWire::default();
        wire.persisted.lock().expect("lock").insert(
            "s2".to_owned(),
            QueryLog {
                header: header_for("s2", None),
                inherited_event_count: 0,
                events: vec![user_event(0, "stored")],
            },
        );
        let engine = engine(wire);
        let live_lookup = |_id: &str| -> Option<QueryLog> { None };
        let results = engine.read_title_snapshots(
            &["s1".to_owned(), "s2".to_owned(), "s1".to_owned()],
            &live_lookup,
        );
        assert_eq!(results.len(), 2, "the batch answers one row per unique id");
        let names: Vec<String> = results
            .iter()
            .map(|result| match result {
                ProjectionResult::Fulfilled { session_id, .. }
                | ProjectionResult::Rejected { session_id, .. } => session_id.clone(),
            })
            .collect();
        assert_eq!(names, vec!["s1", "s2"]);
        assert!(matches!(
            &results[0],
            ProjectionResult::Rejected { reason, .. }
                if reason.code == SessionQueryErrorCode::SessionNotFound
        ));
        assert!(matches!(&results[1], ProjectionResult::Fulfilled { .. }));
    }

    #[test]
    fn surfaces_and_documents_classify_the_fold() {
        let events = vec![
            user_event(0, "first"),
            json!({
                "seq": 1, "time": 2, "type": "user/message",
                "surfaceOp": { "op": "replace", "startSeq": 0, "endSeq": 0 },
                "sourceEventSeqs": [0],
                "data": { "role": "user", "id": "usr-1",
                          "content": [{ "type": "text", "text": "second" }],
                          "source": { "kind": "user" } },
            }),
            json!({
                "seq": 2, "time": 3, "type": "turn/start",
                "data": { "turn": 1 },
            }),
        ];
        let typed = serde_json::from_value::<Vec<SessionEvent>>(Value::Array(events))
            .expect("events decode");
        let surface = current_surface_events("s1", &typed).expect("surface");
        assert_eq!(surface.len(), 1);
        assert_eq!(
            extract_session_event_text(&surface[0]),
            "second",
            "the shadowed node leaves the surface"
        );
        let documents = build_session_event_search_documents("s1", &typed).expect("documents");
        // The shadowed user text stays searchable; the structural
        // turn/start omits.
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].surface, "shadowed");
        assert_eq!(documents[1].surface, "current");
        assert_eq!(documents[0].text, "first");
        assert_eq!(documents[1].text, "second");
    }
}
