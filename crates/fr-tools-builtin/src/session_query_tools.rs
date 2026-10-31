// Rust guideline compliant 2026-10-31
//! The five model-facing session-query tools (dsh
//! `@deepseek-ai/dsh-tool-session-query` 1:1).
//!
//! The bodies cover workspace-authorized search and read, the
//! model-safe service-boundary translation, and the text
//! presentation.
//!
//! Q16d-1A: the six tools register unconditionally (the fr headless
//! directory is a superset of the dsh base).
//!
//! Q16d-2A: the two search bodies reach the engine's disabled terminal
//! state after the same input normalization, filter construction, and
//! parent authorization the dsh bodies run first.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::session_query::signed_to_f64;
use crate::session_query::{
    EventFilter, LineageNode, SessionFilter, SessionQueryEngine, SessionQueryError,
    SessionQueryErrorCode, header_cwd, header_id,
};
use crate::session_query_time::ExactTimestamp;
use crate::tools::BodyError;

use SessionQueryErrorCode as Code;

/// The baked search cap (the dsh `Config.maxSearchResults` default; the
/// headless bundle adds no tool-session-query config row).
pub const BAKED_MAX_SEARCH_RESULTS: u64 = 100;
/// The baked cooperative search deadline in milliseconds (the dsh
/// `Config.searchTimeoutMs` default).
pub const BAKED_SEARCH_TIMEOUT_MS: i64 = 30_000;

/// The shared prompt-section order (the generated orders table's
/// `TOOL_SESSION_QUERY` row).
pub const SECTION_ORDER: f64 = 2300.0;

// ---------------------------------------------------------------------------
// Service boundary (dsh service-boundary.ts)
// ---------------------------------------------------------------------------

/// One model-safe service failure (the code and the printed text).
struct SafeFailure {
    code: SessionQueryErrorCode,
    message: &'static str,
    /// The swallowed rows print the generic failure instead (dsh
    /// `SESSION_QUERY_TOOL_FAILED`).
    swallow: bool,
}

/// The model-safe translation table (dsh `SAFE_SESSION_QUERY_FAILURES`;
/// every closed code member carries its row).
const fn safe_failure(table_code: SessionQueryErrorCode) -> SafeFailure {
    match table_code {
        Code::Aborted => SafeFailure {
            code: Code::Aborted,
            message: "session query was cancelled",
            swallow: false,
        },
        Code::CorruptSession => SafeFailure {
            code: Code::CorruptSession,
            message: "session event history is corrupt",
            swallow: false,
        },
        Code::EventNotFound => SafeFailure {
            code: Code::EventNotFound,
            message: "session event was not found",
            swallow: false,
        },
        Code::IndexFailed => SafeFailure {
            code: Code::IndexFailed,
            message: "session search index is unavailable",
            swallow: false,
        },
        Code::InvalidConfig => SafeFailure {
            code: Code::InvalidConfig,
            message: "session query operation failed",
            swallow: true,
        },
        Code::InvalidCursor => SafeFailure {
            code: Code::InvalidCursor,
            message: "session search continuation is invalid",
            swallow: false,
        },
        Code::InvalidFilter => SafeFailure {
            code: Code::InvalidFilter,
            message: "session query filters were rejected",
            swallow: false,
        },
        Code::InvalidLimit => SafeFailure {
            code: Code::InvalidLimit,
            message: "session query result limit was rejected",
            swallow: false,
        },
        Code::InvalidQuery => SafeFailure {
            code: Code::InvalidQuery,
            message: "session query was rejected",
            swallow: false,
        },
        Code::InvalidLineage => SafeFailure {
            code: Code::InvalidLineage,
            message: "session lineage is invalid",
            swallow: false,
        },
        Code::InvalidSurface => SafeFailure {
            code: Code::InvalidSurface,
            message: "session event history is invalid",
            swallow: false,
        },
        Code::InvalidWindow => SafeFailure {
            code: Code::InvalidWindow,
            message: "session event window is invalid",
            swallow: false,
        },
        Code::PersistenceFailed => SafeFailure {
            code: Code::PersistenceFailed,
            message: "session history storage is unavailable",
            swallow: false,
        },
        Code::SearchDisabled => SafeFailure {
            code: Code::SearchDisabled,
            message: "session search is disabled in this deployment",
            swallow: false,
        },
        Code::SessionNotFound => SafeFailure {
            code: Code::SessionNotFound,
            message: "session was not found",
            swallow: false,
        },
        Code::StaleCursor => SafeFailure {
            code: Code::StaleCursor,
            message: "session history changed while paging; retry the complete search call",
            swallow: false,
        },
        Code::SourceConflict => SafeFailure {
            code: Code::SourceConflict,
            message: "session query operation failed",
            swallow: true,
        },
    }
}

/// The unauthorized-target refusal (dsh `unauthorizedTarget`).
#[must_use]
pub fn unauthorized_target() -> BodyError {
    BodyError {
        code: Some("SESSION_QUERY_TOOL_UNAUTHORIZED".to_owned()),
        name: "HarnessError".to_owned(),
        message: "session target is outside the caller workspace".to_owned(),
    }
}

/// The generic refusal (dsh `genericFailure`).
#[must_use]
fn generic_failure() -> BodyError {
    BodyError {
        code: Some("SESSION_QUERY_TOOL_FAILED".to_owned()),
        name: "HarnessError".to_owned(),
        message: "session query operation failed".to_owned(),
    }
}

/// Translates one service failure onto the model-safe body shape (dsh
/// `sanitizeError`).
///
/// The closed-table rows pass their safe wording, the swallowed rows
/// collapse onto the generic, the tool-unauthorized error re-renders,
/// everything else genericizes.
#[must_use]
pub fn sanitize_error(error: &SessionQueryError) -> BodyError {
    let failure = safe_failure(error.code);
    if !failure.swallow {
        return BodyError {
            code: Some(failure.code.as_str().to_owned()),
            name: "SessionQueryError".to_owned(),
            message: failure.message.to_owned(),
        };
    }
    generic_failure()
}

/// Runs one service operation through the boundary (dsh
/// `serviceBoundary.call`): the safe body replaces the full error text.
///
/// The dsh `logger.warn` diagnostic line has no M1 wire seam (the dylib
/// carries no logger face); the operation name stays in the signature
/// for the Phase 18 diagnostics wiring.
///
/// # Errors
///
/// The sanitized body error.
pub fn call<ValueOut>(
    operation: &str,
    invoke: impl FnOnce() -> Result<ValueOut, SessionQueryError>,
) -> Result<ValueOut, BodyError> {
    let _ = operation;
    invoke().map_err(|error| sanitize_error(&error))
}

// ---------------------------------------------------------------------------
// Workspace access (dsh workspace-access.ts)
// ---------------------------------------------------------------------------

/// The runtime the five bodies share (the engine plus the resolved
/// search bounds).
#[derive(Debug)]
pub struct SessionQueryRuntime {
    /// The engine (the provided 'session-query' service's core).
    pub engine: Arc<SessionQueryEngine>,
    /// The authorized search cap (dsh `Config.maxSearchResults`).
    pub max_search_results: u64,
}

/// The immutable caller identity (dsh `Caller`): the agent id, the
/// session header, and the caller's own turn-boundary fold.
#[derive(Debug, Clone)]
pub struct Caller {
    /// The agent's session id.
    pub id: String,
    /// The session header (the live read's header).
    pub header: Value,
    /// The caller's own `turnBoundary` projection state.
    pub boundary: Option<Value>,
}

/// The turn-boundary projection key (the dsh `stateOf` word).
pub const TURN_BOUNDARY_KEY: &str = "turnBoundary";

/// Resolves the agent-bound caller (dsh `callerOf`): an absent agent
/// refuses; the header reads off the live store.
///
/// # Errors
///
/// The missing-agent refusal and the caller read's transport family.
pub fn caller_of(agent: Option<&str>, runtime: &SessionQueryRuntime) -> Result<Caller, BodyError> {
    let Some(agent) = agent else {
        return Err(BodyError {
            code: Some("SESSION_QUERY_TOOL_MISSING_AGENT".to_owned()),
            name: "HarnessError".to_owned(),
            message: "session query tools require an agent-bound caller".to_owned(),
        });
    };
    let wire = runtime.engine.wire();
    let read_error = |text: String| sanitize_error(&crate::session_query::transport_error(&text));
    let log = wire
        .session_read(agent)
        .map_err(read_error)?
        .ok_or_else(unauthorized_target)?;
    let boundary = wire
        .projection_state(agent, TURN_BOUNDARY_KEY)
        .map_err(read_error)?;
    Ok(Caller {
        id: agent.to_owned(),
        header: log.header,
        boundary,
    })
}

/// Resolves the target id (dsh `targetId`: the omitted id falls to the
/// caller).
#[must_use]
pub fn target_id(args: &Value, caller: &Caller) -> String {
    args.get("session_id")
        .and_then(Value::as_str)
        .map_or_else(|| caller.id.clone(), str::to_owned)
}

/// Authorizes one target (dsh `authorizeTarget`: the caller's own id
/// passes; any other target must exist in the caller's workspace
/// corpus).
///
/// # Errors
///
/// The unauthorized and listing refusals.
pub fn authorize_target(
    caller: &Caller,
    target: &str,
    runtime: &SessionQueryRuntime,
) -> Result<(), BodyError> {
    if target == caller.id {
        return Ok(());
    }
    let Some(cwd) = header_cwd(&caller.header) else {
        return Err(unauthorized_target());
    };
    let records = call("target authorization", || {
        runtime.engine.filter_sessions(
            &[
                SessionFilter::Id {
                    values: vec![target.to_owned()],
                },
                SessionFilter::Cwd {
                    values: vec![Some(cwd.to_owned())],
                },
            ],
            std::slice::from_ref(&caller.id),
        )
    })?;
    if records.len() != 1 {
        return Err(unauthorized_target());
    }
    Ok(())
}

/// Whether one record header is visible from the caller (dsh
/// `headerAuthorized`: the caller's own id must match its cwd; other
/// headers need the caller cwd and an equal cwd).
#[must_use]
pub fn header_authorized(header: &Value, caller: &Caller) -> bool {
    let same_cwd = header_cwd(header) == header_cwd(&caller.header);
    if header_id(header) == Some(caller.id.as_str()) {
        return same_cwd;
    }
    header_cwd(&caller.header).is_some() && same_cwd
}

/// Whether one corpus record is visible (dsh `recordAuthorized`).
#[must_use]
pub fn record_authorized(record: &crate::session_query::SessionRecord, caller: &Caller) -> bool {
    header_authorized(&record.header, caller)
}

/// Re-checks the observed target after a read (dsh
/// `assertObservedTargetAuthorized`).
///
/// # Errors
///
/// The unauthorized refusal when the observed header differs.
pub fn assert_observed_target_authorized(
    caller: &Caller,
    target: &str,
    observed: &Value,
) -> Result<(), BodyError> {
    if header_id(observed) != Some(target) || !header_authorized(observed, caller) {
        return Err(unauthorized_target());
    }
    Ok(())
}

/// Authorizes a candidate id set (dsh `authorizeSessionIds`: the caller
/// id always passes; the rest must exist in the workspace corpus).
///
/// # Errors
///
/// The listing refusal.
pub fn authorize_session_ids(
    caller: &Caller,
    ids: &[String],
    runtime: &SessionQueryRuntime,
) -> Result<HashSet<String>, BodyError> {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        if seen.insert(id.clone()) {
            unique.push(id.clone());
        }
    }
    let mut authorized: HashSet<String> = HashSet::new();
    if unique.contains(&caller.id) {
        authorized.insert(caller.id.clone());
    }
    let Some(cwd) = header_cwd(&caller.header) else {
        return Ok(authorized);
    };
    let other: Vec<String> = unique.into_iter().filter(|id| *id != caller.id).collect();
    if other.is_empty() {
        return Ok(authorized);
    }
    let records = call("session-id authorization", || {
        runtime.engine.filter_sessions(
            &[
                SessionFilter::Id { values: other },
                SessionFilter::Cwd {
                    values: vec![Some(cwd.to_owned())],
                },
            ],
            std::slice::from_ref(&caller.id),
        )
    })?;
    for record in records {
        if record_authorized(&record, caller)
            && let Some(id) = header_id(&record.header)
        {
            authorized.insert(id.to_owned());
        }
    }
    Ok(authorized)
}

/// One title view (dsh `TitleView`).
#[derive(Debug, Clone)]
pub struct TitleView {
    /// The folded title text.
    pub text: String,
    /// The safe code when the title observation failed.
    pub unavailable_code: Option<String>,
}

/// Reads the title views for one id batch (dsh `readTitles`): the
/// per-session failures isolate into the unavailable view; the
/// unauthorized item failure re-raises.
///
/// # Errors
///
/// The batch-level transport family.
pub fn read_titles(
    caller: &Caller,
    ids: &[String],
    runtime: &SessionQueryRuntime,
) -> Result<std::collections::HashMap<String, TitleView>, BodyError> {
    let live_lookup = live_lookup(caller, runtime);
    let observations = runtime.engine.read_title_snapshots(ids, &live_lookup);
    let mut titles = std::collections::HashMap::new();
    for observation in observations {
        match observation {
            crate::session_query::ProjectionResult::Fulfilled { session_id, value } => {
                assert_observed_target_authorized(caller, &session_id, &value.session)?;
                let text = value
                    .title
                    .as_ref()
                    .and_then(|title| title.get("title").and_then(Value::as_str))
                    .unwrap_or("untitled")
                    .to_owned();
                titles.insert(
                    session_id,
                    TitleView {
                        text,
                        unavailable_code: None,
                    },
                );
            }
            crate::session_query::ProjectionResult::Rejected { session_id, reason } => {
                let sanitized = sanitize_error(&reason);
                if sanitized.code.as_deref() == Some("SESSION_QUERY_TOOL_UNAUTHORIZED") {
                    return Err(unauthorized_target());
                }
                titles.insert(
                    session_id,
                    TitleView {
                        text: "untitled".to_owned(),
                        unavailable_code: sanitized.code,
                    },
                );
            }
        }
    }
    Ok(titles)
}

/// Reads one title view (dsh `readTitle`).
///
/// # Errors
///
/// The batch-level transport family.
pub fn read_title(
    caller: &Caller,
    id: &str,
    runtime: &SessionQueryRuntime,
) -> Result<TitleView, BodyError> {
    let titles = read_titles(caller, std::slice::from_ref(&id.to_owned()), runtime)?;
    Ok(titles.get(id).cloned().unwrap_or_else(|| TitleView {
        text: "untitled".to_owned(),
        unavailable_code: None,
    }))
}

/// The live-source lookup the batch reads pass the engine (the dsh
/// `ctx.sessions.get` per id; M1 = the caller's own live session).
pub fn live_lookup<'a>(
    caller: &'a Caller,
    runtime: &'a SessionQueryRuntime,
) -> impl Fn(&str) -> Option<crate::session_query::QueryLog> + 'a {
    move |id: &str| {
        if id != caller.id {
            return None;
        }
        runtime.engine.wire().session_read(id).ok().flatten()
    }
}

/// The projected descendant node (dsh `AuthorizedDescendant`).
#[derive(Debug, Clone)]
pub struct AuthorizedDescendant {
    /// The visible record.
    pub record: crate::session_query::SessionRecord,
    /// The visible subtree.
    pub descendants: Vec<Option<Self>>,
}

/// Projects the descendant trees onto caller visibility (dsh
/// `authorizeDescendants`: invisible nodes collapse to `None`).
#[must_use]
pub fn authorize_descendants(
    nodes: &[LineageNode],
    caller: &Caller,
) -> Vec<Option<AuthorizedDescendant>> {
    fn extend(nodes: &[LineageNode], caller: &Caller) -> Vec<Option<AuthorizedDescendant>> {
        nodes
            .iter()
            .map(|node| {
                if !record_authorized(&node.session, caller) {
                    return None;
                }
                Some(AuthorizedDescendant {
                    record: node.session.clone(),
                    descendants: extend(&node.descendants, caller),
                })
            })
            .collect()
    }
    extend(nodes, caller)
}

/// One visit step (dsh `DescendantVisit`).
#[derive(Debug, Clone)]
pub struct DescendantVisit<'a> {
    /// The visible node, or `None` for an invisible subtree marker.
    pub node: Option<&'a AuthorizedDescendant>,
    /// The render indent depth.
    pub depth: usize,
}

/// Walks the projected trees depth-first (dsh `visitDescendants`).
#[must_use]
pub fn visit_descendants(nodes: &[Option<AuthorizedDescendant>]) -> Vec<DescendantVisit<'_>> {
    fn extend<'a>(
        nodes: &'a [Option<AuthorizedDescendant>],
        depth: usize,
        visits: &mut Vec<DescendantVisit<'a>>,
    ) {
        for node in nodes {
            visits.push(DescendantVisit {
                node: node.as_ref(),
                depth,
            });
            if let Some(node) = node {
                extend(&node.descendants, depth + 1, visits);
            }
        }
    }
    let mut visits: Vec<DescendantVisit<'_>> = Vec::new();
    extend(nodes, 0, &mut visits);
    visits
}

/// Collects the visible descendant ids (dsh `descendantIds`).
#[must_use]
pub fn descendant_ids(nodes: &[Option<AuthorizedDescendant>]) -> Vec<String> {
    visit_descendants(nodes)
        .into_iter()
        .filter_map(|visit| {
            visit
                .node
                .map(|node| crate::session_query::record_id(&node.record))
        })
        .collect()
}

/// Renders one title view (dsh `titleText`: the unavailable code rides
/// the parenthetical).
#[must_use]
pub fn title_text(view: &TitleView) -> String {
    view.unavailable_code.as_ref().map_or_else(
        || view.text.clone(),
        |code| format!("{} (title unavailable: {code})", view.text),
    )
}

// ---------------------------------------------------------------------------
// Input (dsh input.ts)
// ---------------------------------------------------------------------------

/// Normalizes the query text (dsh `normalizeQuery`: trim, collapse
/// whitespace runs, refuse blank and NUL — the dsh texts verbatim).
///
/// # Errors
///
/// The `SESSION_QUERY_INVALID_QUERY` refusals.
pub fn normalize_query(value: &str) -> Result<String, BodyError> {
    let query = collapse_whitespace(value);
    if query.is_empty() {
        return Err(query_error(
            SessionQueryErrorCode::InvalidQuery,
            "session-search query must contain non-whitespace text",
        ));
    }
    if query.contains('\u{0}') {
        return Err(query_error(
            SessionQueryErrorCode::InvalidQuery,
            "session-search query must not contain NUL",
        ));
    }
    Ok(query)
}

/// Collapses whitespace runs into single spaces (dsh `\s+` replace).
#[must_use]
pub fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Builds one session-query-shaped error body (the
/// `SessionQueryError` name carries).
#[must_use]
pub fn query_error(code: SessionQueryErrorCode, message: &str) -> BodyError {
    SessionQueryError::new(code, message).to_body()
}

/// Builds the session filters from the raw arguments (dsh
/// `buildSessionFilters`: id/created-at/availability clauses in the dsh
/// clause order, empty arrays refuse).
///
/// # Errors
///
/// The filter-shape refusals.
pub fn build_session_filters(args: &Value) -> Result<Vec<SessionFilter>, BodyError> {
    let mut filters: Vec<SessionFilter> = Vec::new();
    if let Some(values) = args.get("session_ids").and_then(Value::as_array) {
        assert_non_empty_array("session_ids", values)?;
        filters.push(SessionFilter::Id {
            values: values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect(),
        });
    }
    let from = args.get("created_at_from").and_then(Value::as_str);
    let to = args.get("created_at_to").and_then(Value::as_str);
    if from.is_some() || to.is_some() {
        let (lower, upper) = timestamp_range("created_at", from, to)?;
        filters.push(SessionFilter::CreatedAt {
            from: lower,
            to: upper,
        });
    }
    if let Some(values) = args.get("availability").and_then(Value::as_array) {
        assert_non_empty_array("availability", values)?;
        let mut words: Vec<&'static str> = Vec::new();
        for value in values {
            let word = value.as_str().ok_or_else(|| {
                query_error(
                    SessionQueryErrorCode::InvalidFilter,
                    "session availability filter values must be an array of strings",
                )
            })?;
            match word {
                "live" => words.push("live"),
                "persisted" => words.push("persisted"),
                other => {
                    return Err(query_error(
                        SessionQueryErrorCode::InvalidFilter,
                        &format!("session availability filter contains unknown value \"{other}\""),
                    ));
                }
            }
        }
        filters.push(SessionFilter::Availability { values: words });
    }
    Ok(filters)
}

/// Deduplicates the parent candidates (dsh
/// `materializeParentSessionIds`; empty arrays refuse).
///
/// # Errors
///
/// The filter-shape refusal.
pub fn materialize_parent_session_ids(args: &Value) -> Option<Result<Vec<String>, BodyError>> {
    let values = args.get("parent_session_ids").and_then(Value::as_array)?;
    let outcome = (|| {
        assert_non_empty_array("parent_session_ids", values)?;
        let mut unique: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for value in values {
            if let Some(text) = value.as_str()
                && seen.insert(text.to_owned())
            {
                unique.push(text.to_owned());
            }
        }
        Ok(unique)
    })();
    Some(outcome)
}

/// Builds the event metadata filters (dsh `buildEventFilters`: seq and
/// time ranges plus the type/surface memberships in the dsh clause
/// order).
///
/// # Errors
///
/// The filter-shape refusals.
pub fn build_event_filters(input: &Value) -> Result<Vec<EventFilter>, BodyError> {
    let mut filters: Vec<EventFilter> = Vec::new();
    let from = input.get("seqFrom").and_then(Value::as_f64);
    let to = input.get("seqTo").and_then(Value::as_f64);
    if from.is_some() || to.is_some() {
        sequence_range_pair(from, to)?;
        filters.push(EventFilter::Seq { from, to });
    }
    let time_from = input.get("timeFrom").and_then(Value::as_str);
    let time_to = input.get("timeTo").and_then(Value::as_str);
    if time_from.is_some() || time_to.is_some() {
        let (lower, upper) = timestamp_range("time", time_from, time_to)?;
        filters.push(EventFilter::Time {
            from: lower,
            to: upper,
        });
    }
    if let Some(values) = input.get("eventTypes").and_then(Value::as_array) {
        assert_non_empty_array("event_types", values)?;
        filters.push(EventFilter::Type {
            values: values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect(),
        });
    }
    if let Some(values) = input.get("surfaces").and_then(Value::as_array) {
        assert_non_empty_array("surfaces", values)?;
        let mut words: Vec<String> = Vec::new();
        for value in values {
            let word = value.as_str().ok_or_else(|| {
                query_error(
                    SessionQueryErrorCode::InvalidFilter,
                    "session surfaces filter values must be an array of strings",
                )
            })?;
            match word {
                "current" | "shadowed" | "log-only" => words.push(word.to_owned()),
                other => {
                    return Err(query_error(
                        SessionQueryErrorCode::InvalidFilter,
                        &format!("session surfaces filter contains unknown value \"{other}\""),
                    ));
                }
            }
        }
        filters.push(EventFilter::Surface { values: words });
    }
    Ok(filters)
}

/// Validates one inclusive numeric pair (dsh `sequenceRange` bounds).
///
/// # Errors
///
/// The dsh refusal texts, verbatim.
pub fn sequence_range_pair(from: Option<f64>, to: Option<f64>) -> Result<(), BodyError> {
    if let Some(from) = from {
        assert_non_negative_safe_integer("sequence lower bound", from)?;
    }
    if let Some(to) = to {
        assert_non_negative_safe_integer("sequence upper bound", to)?;
    }
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(query_error(
            SessionQueryErrorCode::InvalidFilter,
            "session sequence range from must be less than or equal to to",
        ));
    }
    Ok(())
}

/// Parses the timestamp pair into the inclusive f64 bounds (dsh
/// `timestampRange`: the sub-millisecond remainders step the bounds by
/// one float ULP; the dsh refusal texts ride the range family).
///
/// # Errors
///
/// The range refusals.
pub fn timestamp_range(
    name: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(Option<f64>, Option<f64>), BodyError> {
    let parse_leg = |leg: &str, text: &str| -> Result<ExactTimestamp, BodyError> {
        crate::session_query_time::parse_iso_timestamp(text).map_err(|detail| {
            query_error(
                SessionQueryErrorCode::InvalidFilter,
                &format!("session {name} range {leg} {detail}"),
            )
        })
    };
    let from_ts = from.map(|text| parse_leg("from", text)).transpose()?;
    let to_ts = to.map(|text| parse_leg("to", text)).transpose()?;
    if let (Some(from_ts), Some(to_ts)) = (&from_ts, &to_ts)
        && compare_timestamps(from_ts, to_ts) == std::cmp::Ordering::Greater
    {
        return Err(query_error(
            SessionQueryErrorCode::InvalidFilter,
            &format!("session {name} range from must be less than or equal to to"),
        ));
    }
    Ok((
        from_ts.as_ref().map(ExactTimestamp::lower_bound),
        to_ts.as_ref().map(ExactTimestamp::upper_bound),
    ))
}

/// Converts one validated non-negative safe integer into the seq
/// domain (the dsh `SessionSeq(args.seq)` coercion: the shape asserts
/// already refused every fractional or out-of-domain value).
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the shape asserts precede the call, so the double carries an in-domain integer"
)]
pub const fn validated_seq(value: f64) -> u64 {
    value as u64
}

/// Compares two exact timestamps (dsh `compareTimestamps`: the whole
/// milliseconds, then the digit-string remainders). (dsh `compareTimestamps`: the whole
/// milliseconds, then the digit-string remainders).
#[must_use]
pub fn compare_timestamps(left: &ExactTimestamp, right: &ExactTimestamp) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if left.millisecond != right.millisecond {
        return left.millisecond.cmp(&right.millisecond);
    }
    let length = left.remainder.len().max(right.remainder.len());
    for index in 0..length {
        let left_digit = left.remainder[index..].chars().next().unwrap_or('0');
        let right_digit = right.remainder[index..].chars().next().unwrap_or('0');
        if left_digit != right_digit {
            return left_digit.cmp(&right_digit);
        }
    }
    Ordering::Equal
}

/// The non-negative safe-integer assert (dsh
/// `assertNonNegativeSafeInteger`, the dsh text verbatim).
///
/// # Errors
///
/// The dsh refusal.
pub fn assert_non_negative_safe_integer(name: &str, value: f64) -> Result<(), BodyError> {
    let in_domain = value.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(&value);
    if !in_domain {
        return Err(query_error(
            SessionQueryErrorCode::InvalidFilter,
            &format!("{name} must be a non-negative safe integer"),
        ));
    }
    Ok(())
}

/// The non-empty array assert (dsh `assertNonEmptyArray`, the dsh text
/// verbatim).
///
/// # Errors
///
/// The dsh refusal.
pub fn assert_non_empty_array(name: &str, values: &[Value]) -> Result<(), BodyError> {
    if values.is_empty() {
        return Err(query_error(
            SessionQueryErrorCode::InvalidFilter,
            &format!("{name} must contain at least one value when supplied"),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Presentation (dsh presentation.ts)
// ---------------------------------------------------------------------------

/// Renders one epoch millisecond as the dsh `toISOString` text (the
/// dsh `formatTime(value: number)`: the double truncates to the whole
/// millisecond exactly the way `new Date(ms)` does).
#[must_use]
pub fn format_time(value: f64) -> String {
    crate::session_query_time::to_iso_string(dsh_epoch_millisecond(value))
}

/// Truncates one dsh epoch number into the whole millisecond (the
/// `new Date(value)` coercion).
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the dsh Date coercion truncates the double into the millisecond domain"
)]
pub const fn dsh_epoch_millisecond(value: f64) -> i64 {
    value as i64
}

/// The empty session-search text (dsh `formatEmptySessionSearch`).
#[must_use]
pub fn format_empty_session_search() -> String {
    "No prior session matches found.".to_owned()
}

/// The empty event-search text (dsh `formatEventSearch`'s empty arm;
/// reachable through the degenerate-range early return).
#[must_use]
pub fn format_empty_event_search() -> String {
    "No prior event matches found.".to_owned()
}

/// Formats one session trace (dsh `formatSessionTrace`: the target
/// block, the nearest-first ancestors with the outside-boundary marker,
/// and the descendant tree in two-space indents).
#[must_use]
pub fn format_session_trace<S: std::hash::BuildHasher>(
    trace: &crate::session_query::LineageTrace,
    visible_ancestors: &[crate::session_query::SessionRecord],
    ancestor_boundary: bool,
    descendants: &[Option<AuthorizedDescendant>],
    titles: &std::collections::HashMap<String, TitleView, S>,
) -> String {
    let mut lines: Vec<String> = vec![
        format!(
            "Session {} — {}",
            crate::session_query::record_id(&trace.target),
            title_text_for_id(titles, &crate::session_query::record_id(&trace.target)),
        ),
        format!(
            "Created: {}",
            format_time(
                crate::session_query::header_created_at(&trace.target.header).unwrap_or(0.0)
            ),
        ),
        format!("Availability: {}", availability_text(&trace.target),),
        String::new(),
        "Ancestors (nearest first):".to_owned(),
    ];
    if visible_ancestors.is_empty() && !ancestor_boundary {
        lines.push("- none (target is a root session)".to_owned());
    }
    for record in visible_ancestors {
        let id = crate::session_query::record_id(record);
        lines.push(format!(
            "- {id} — {} | {} | {}",
            title_text_for_id(titles, &id),
            format_time(crate::session_query::header_created_at(&record.header).unwrap_or(0.0)),
            availability_text(record),
        ));
    }
    if ancestor_boundary {
        lines.push("- [outside workspace boundary]".to_owned());
    }
    lines.push(String::new());
    lines.push("Descendants:".to_owned());
    let visits = visit_descendants(descendants);
    if visits.is_empty() {
        lines.push("- none".to_owned());
    } else {
        for visit in visits {
            let indent = "  ".repeat(visit.depth);
            let Some(node) = visit.node else {
                lines.push(format!("{indent}- [outside workspace subtree]"));
                continue;
            };
            let id = crate::session_query::record_id(&node.record);
            lines.push(format!(
                "{indent}- {id} — {} | {} | {}",
                title_text_for_id(titles, &id),
                format_time(
                    crate::session_query::header_created_at(&node.record.header).unwrap_or(0.0)
                ),
                availability_text(&node.record),
            ));
        }
    }
    lines.join("\n")
}

/// One record's availability words (dsh `availabilityText`).
#[must_use]
pub fn availability_text(record: &crate::session_query::SessionRecord) -> String {
    let mut words: Vec<&str> = Vec::new();
    if record.live {
        words.push("live");
    }
    if record.persisted {
        words.push("persisted");
    }
    if words.is_empty() {
        return "unavailable".to_owned();
    }
    words.join(", ")
}

/// Reads one title view for the render (the absent id falls to the
/// bare word).
#[must_use]
fn title_text_for_id<S: std::hash::BuildHasher>(
    titles: &std::collections::HashMap<String, TitleView, S>,
    id: &str,
) -> String {
    titles
        .get(id)
        .map_or_else(|| "untitled".to_owned(), title_text)
}

/// Formats one event trace (dsh `formatEventTrace`: the seven-line
/// block, the absent chains reading `none`).
#[must_use]
pub fn format_event_trace(
    session_id: &str,
    title: &TitleView,
    trace: &crate::session_query::EventTrace,
) -> String {
    [
        format!("Session {session_id} — {}", title_text(title)),
        format!(
            "Target: seq {} | {} | {} | {}",
            trace.target.seq,
            trace.target.event_type,
            trace.target.surface,
            format_time(signed_to_f64(trace.target.time)),
        ),
        format!(
            "Replaced by: {}",
            trace
                .replaced_by
                .map_or_else(|| "none".to_owned(), |seq| seq.to_string()),
        ),
        format!("Replacement chain: {}", seq_list(&trace.replacement_chain)),
        format!(
            "Events replaced by target: {}",
            seq_list(&trace.replaced_event_seqs)
        ),
        format!(
            "Events cited directly as sources: {}",
            seq_list(&trace.source_event_seqs)
        ),
        format!(
            "Direct derived events: {}",
            seq_list(&trace.derived_event_seqs)
        ),
    ]
    .join("\n")
}

/// Formats one event read (dsh `formatEventRead`: the full target in
/// the json fence and the before/after neighbor summaries).
#[must_use]
pub fn format_event_read(
    session_id: &str,
    title: &TitleView,
    window: &crate::session_query::EventWindow,
) -> String {
    let target_seq = window.target.seq.get();
    let before: Vec<&fr_wire::event::SessionEvent> = window
        .events
        .iter()
        .filter(|event| event.seq.get() < target_seq)
        .collect();
    let after: Vec<&fr_wire::event::SessionEvent> = window
        .events
        .iter()
        .filter(|event| event.seq.get() > target_seq)
        .collect();
    let mut lines: Vec<String> = vec![
        format!("Session {session_id} — {}", title_text(title)),
        format!("Target event seq {target_seq}:"),
        "```json".to_owned(),
        serde_json::to_string_pretty(&window.target).unwrap_or_default(),
        "```".to_owned(),
    ];
    if !before.is_empty() {
        lines.push(String::new());
        lines.push("Before:".to_owned());
        for event in before {
            lines.push(format_neighbor(event));
        }
    }
    if !after.is_empty() {
        lines.push(String::new());
        lines.push("After:".to_owned());
        for event in after {
            lines.push(format_neighbor(event));
        }
    }
    lines.join("\n")
}

/// Formats one neighbor event (dsh `formatNeighbor`: the seq row plus
/// the two-space-indented semantic text, `(no semantic text)` when the
/// extraction is empty).
#[must_use]
pub fn format_neighbor(event: &fr_wire::event::SessionEvent) -> String {
    let text = crate::session_query::extract_session_event_text(event);
    if text.is_empty() {
        format!(
            "- seq {} | {} | {} | (no semantic text)",
            event.seq.get(),
            event.event_type(),
            format_time(signed_to_f64(event.time)),
        )
    } else {
        format!(
            "- seq {} | {} | {}\n  {}",
            event.seq.get(),
            event.event_type(),
            format_time(signed_to_f64(event.time)),
            text.replace('\n', "\n  "),
        )
    }
}

/// Renders one seq list (dsh `seqList`: empty reads `none`).
#[must_use]
pub fn seq_list(values: &[u64]) -> String {
    if values.is_empty() {
        return "none".to_owned();
    }
    values
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formats the event-search header (dsh `formatEventSearch`'s header
/// line; the empty arm rides the early return before this call).
#[must_use]
pub fn format_event_search_header(session_id: &str, title: &TitleView) -> String {
    format!("Session {session_id} — {}", title_text(title))
}

/// Projects one search-call card (dsh `presentSessionSearchCall`).
#[must_use]
pub fn present_session_search_call(args: &Value) -> Value {
    json!({
        "card": "generic",
        "kind": "search",
        "title": "Search prior sessions",
        "rawInput": args.get("query").cloned().unwrap_or(Value::Null),
    })
}

/// Projects one event-search call card (dsh `presentEventSearchCall`).
#[must_use]
pub fn present_event_search_call(args: &Value) -> Value {
    json!({
        "card": "generic",
        "kind": "search",
        "title": "Search session events",
        "rawInput": args.get("query").cloned().unwrap_or(Value::Null),
    })
}

/// Projects one session-trace call card (dsh `presentSessionTraceCall`:
/// the omitted id reads the current session).
#[must_use]
pub fn present_session_trace_call(args: &Value) -> Value {
    let session_id = args.get("session_id").and_then(Value::as_str);
    let title = session_id.map_or_else(
        || "Trace current session".to_owned(),
        |id| format!("Trace session {id}"),
    );
    let mut card = json!({
        "card": "generic",
        "kind": "read",
        "title": title,
    });
    if let Some(id) = session_id {
        card["rawInput"] = json!(id);
    }
    card
}

/// Projects one event-target call card (dsh `presentEventTargetCall`).
#[must_use]
pub fn present_event_target_call(action: &str, args: &Value) -> Value {
    let seq = args.get("seq").cloned().unwrap_or(Value::Null);
    let mut raw_input = serde_json::Map::new();
    if let Some(session_id) = args.get("session_id").filter(|value| !value.is_null()) {
        raw_input.insert("session_id".to_owned(), session_id.clone());
    }
    raw_input.insert("seq".to_owned(), seq);
    json!({
        "card": "generic",
        "kind": "read",
        "title": format!("{action} {}", args.get("seq").and_then(Value::as_u64).unwrap_or(0)),
        "rawInput": Value::Object(raw_input),
    })
}

// ---------------------------------------------------------------------------
// Operations (dsh operations.ts)
// ---------------------------------------------------------------------------

/// Runs one `session_search` call (dsh `executeSessionSearch`).
///
/// The caller workspace gate, the query/filter shapes, the parent
/// authorization, and the corpus-scoped search all run in the dsh
/// order; the disabled terminal state answers the provider leg.
///
/// # Errors
///
/// The caller, filter, and provider refusal families.
pub fn execute_session_search(
    args: &Value,
    agent: Option<&str>,
    runtime: &SessionQueryRuntime,
) -> Result<Value, BodyError> {
    let caller = caller_of(agent, runtime)?;
    if header_cwd(&caller.header).is_none() {
        return Err(BodyError {
            code: Some("SESSION_QUERY_TOOL_UNAUTHORIZED".to_owned()),
            name: "HarnessError".to_owned(),
            message:
                "cross-session search is unavailable because the caller session has no workspace"
                    .to_owned(),
        });
    }
    normalize_query(
        args.get("query")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    build_session_filters(args)?;
    let event_filter_input = json!({
        "seqFrom": args.get("event_seq_from"),
        "seqTo": args.get("event_seq_to"),
        "timeFrom": args.get("event_time_from"),
        "timeTo": args.get("event_time_to"),
        "eventTypes": args.get("event_types"),
        "surfaces": args.get("event_surfaces"),
    });
    // The event-filter shapes validate here (the provider leg consumes
    // them with the search backend slice).
    build_event_filters(&event_filter_input)?;
    let parents = materialize_parent_session_ids(args);
    let include_roots = args
        .get("include_root_sessions")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if parents.is_some() || include_roots {
        let requested = parents.transpose()?.unwrap_or_default();
        let authorized = authorize_session_ids(&caller, &requested, runtime)?;
        let mut parent_count = requested
            .iter()
            .filter(|id| authorized.contains(*id))
            .count();
        if include_roots {
            parent_count += 1;
        }
        if parent_count == 0 {
            return Ok(json!(format_empty_session_search()));
        }
        // The authorized parents plus the root marker compose the
        // parent clause with the backend slice's request assembly.
        // The parent clause joins the session filters here; the search
        // request assembly lands with the backend slice (the disabled
        // terminal state consumes no filters).
    }
    // The workspace clause rides the same request assembly (the cwd
    // gate already refused the absent directory above).
    // The provider leg: the disabled terminal state refuses the first
    // page request (the dsh-base `openAt: 'never'` reachability — the
    // pager never completes, so the hit paging lands with the backend
    // slice).
    call("session search", || runtime.engine.search_sessions())?;
    Ok(json!(format_empty_session_search()))
}

/// Runs one `session_event_search` call (dsh `executeEventSearch`).
///
/// The authorization, the query and range shapes, and the
/// current-session step cap run in the dsh order; the disabled terminal
/// state answers the provider leg.
///
/// # Errors
///
/// The caller, filter, and provider refusal families.
pub fn execute_event_search(
    args: &Value,
    agent: Option<&str>,
    runtime: &SessionQueryRuntime,
) -> Result<Value, BodyError> {
    let caller = caller_of(agent, runtime)?;
    let session_id = target_id(args, &caller);
    authorize_target(&caller, &session_id, runtime)?;
    normalize_query(
        args.get("query")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let range_from = args.get("seq_from").and_then(Value::as_f64);
    let mut range_to = args.get("seq_to").and_then(Value::as_f64);
    sequence_range_pair(range_from, range_to)?;
    if session_id == caller.id {
        // The absent projection marker refuses; the null marker caps
        // the range to a degenerate `to` the filter validation rejects
        // (the loop never runs tools outside a step).
        let Some(step_start) = caller
            .boundary
            .as_ref()
            .and_then(|state| state.get("lastStepStartSeq"))
        else {
            return Err(BodyError {
                code: Some("SESSION_QUERY_TOOL_NO_CURRENT_STEP".to_owned()),
                name: "HarnessError".to_owned(),
                message: "current-session search requires an active step boundary".to_owned(),
            });
        };
        let cap = step_start.as_f64().unwrap_or(0.0) - 1.0;
        range_to = Some(range_to.unwrap_or(9_007_199_254_740_991.0).min(cap));
    }
    read_title(&caller, &session_id, runtime)?;
    if let (Some(from), Some(to)) = (range_from, range_to)
        && from > to
    {
        return Ok(json!(format_empty_event_search()));
    }
    let filter_input = json!({
        "seqFrom": range_from,
        "seqTo": range_to,
        "timeFrom": args.get("time_from"),
        "timeTo": args.get("time_to"),
        "eventTypes": args.get("event_types"),
        "surfaces": args.get("surfaces"),
    });
    // The filter shapes validate here (the provider leg consumes them
    // with the search backend slice).
    build_event_filters(&filter_input)?;
    call("event search", || runtime.engine.search_events())?;
    Ok(json!(format_empty_event_search()))
}

/// Runs one `session_trace` call (dsh `executeSessionTrace`): the
/// authorization, the lineage read, the visible-ancestor walk, and the
/// descendant projection.
///
/// # Errors
///
/// The caller, authorization, and lineage refusal families.
pub fn execute_session_trace(
    args: &Value,
    agent: Option<&str>,
    runtime: &SessionQueryRuntime,
) -> Result<Value, BodyError> {
    let caller = caller_of(agent, runtime)?;
    let session_id = target_id(args, &caller);
    authorize_target(&caller, &session_id, runtime)?;
    let live_overlay = vec![caller.id.clone()];
    let trace = call("session lineage trace", || {
        runtime.engine.trace_session(&session_id, &live_overlay)
    })?;
    assert_observed_target_authorized(&caller, &session_id, &trace.target.header)?;
    let mut visible_ancestors: Vec<crate::session_query::SessionRecord> = Vec::new();
    let mut ancestor_boundary = false;
    for ancestor in &trace.ancestors {
        if !record_authorized(ancestor, &caller) {
            ancestor_boundary = true;
            break;
        }
        visible_ancestors.push(ancestor.clone());
    }
    if visible_ancestors.len() == trace.ancestors.len() && !trace.complete {
        ancestor_boundary = true;
    }
    let descendants = authorize_descendants(&trace.descendants, &caller);
    let mut visible_ids: Vec<String> = vec![session_id];
    for record in &visible_ancestors {
        visible_ids.push(crate::session_query::record_id(record));
    }
    visible_ids.extend(descendant_ids(&descendants));
    let titles = read_titles(&caller, &visible_ids, runtime)?;
    Ok(json!(format_session_trace(
        &trace,
        &visible_ancestors,
        ancestor_boundary,
        &descendants,
        &titles,
    )))
}

/// Runs one `session_event_trace` call (dsh `executeEventTrace`).
///
/// # Errors
///
/// The caller, authorization, and trace refusal families.
pub fn execute_event_trace(
    args: &Value,
    agent: Option<&str>,
    runtime: &SessionQueryRuntime,
) -> Result<Value, BodyError> {
    assert_non_negative_safe_integer(
        "seq",
        args.get("seq").and_then(Value::as_f64).unwrap_or(f64::NAN),
    )?;
    let seq = args.get("seq").and_then(Value::as_f64).unwrap_or(0.0);
    let caller = caller_of(agent, runtime)?;
    let session_id = target_id(args, &caller);
    authorize_target(&caller, &session_id, runtime)?;
    let trace = call("event trace", || {
        runtime.engine.trace_event(&session_id, validated_seq(seq))
    })?;
    assert_observed_target_authorized(&caller, &session_id, &trace.session)?;
    let title = read_title(&caller, &session_id, runtime)?;
    Ok(json!(
        format_event_trace(&session_id, &title, &trace.trace,)
    ))
}

/// Runs one `session_event_read` call (dsh `executeEventRead`).
///
/// # Errors
///
/// The caller, window, authorization, and read refusal families.
pub fn execute_event_read(
    args: &Value,
    agent: Option<&str>,
    runtime: &SessionQueryRuntime,
) -> Result<Value, BodyError> {
    assert_non_negative_safe_integer(
        "seq",
        args.get("seq").and_then(Value::as_f64).unwrap_or(f64::NAN),
    )?;
    if let Some(before) = args.get("before").and_then(Value::as_f64) {
        assert_non_negative_safe_integer("before", before)?;
    }
    if let Some(after) = args.get("after").and_then(Value::as_f64) {
        assert_non_negative_safe_integer("after", after)?;
    }
    let seq = args.get("seq").and_then(Value::as_f64).unwrap_or(0.0);
    let caller = caller_of(agent, runtime)?;
    let session_id = target_id(args, &caller);
    authorize_target(&caller, &session_id, runtime)?;
    let window = call("event read", || {
        runtime.engine.read_event(
            &session_id,
            validated_seq(seq),
            args.get("before")
                .and_then(Value::as_f64)
                .map(validated_seq),
            args.get("after").and_then(Value::as_f64).map(validated_seq),
        )
    })?;
    assert_observed_target_authorized(&caller, &session_id, &window.session)?;
    let title = read_title(&caller, &session_id, runtime)?;
    Ok(json!(format_event_read(&session_id, &title, &window)))
}

/// Renders one finished string value (dsh `TEXT_OUTPUT`: the text block
/// list the registry appends).
#[must_use]
pub fn render_text(_args: &Value, value: &Value) -> Value {
    let text = value.as_str().unwrap_or_default().to_owned();
    json!([{ "type": "text", "text": text }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_query::{EngineConfig, QueryLog, QueryWire};

    /// The scripted wire for the tool bodies: one live session with a
    /// configurable header, and the projection state.
    struct ToolWire {
        header: Value,
        boundary: Option<Value>,
    }

    impl QueryWire for ToolWire {
        fn session_read(&self, id: &str) -> Result<Option<QueryLog>, String> {
            Ok(Some(QueryLog {
                header: {
                    let mut header = self.header.clone();
                    header["id"] = json!(id);
                    header
                },
                inherited_event_count: 0,
                events: Vec::new(),
            }))
        }

        fn persisted_list(&self) -> Result<Vec<Value>, String> {
            Ok(Vec::new())
        }

        fn persisted_cold_read(&self, _id: &str) -> Result<Option<QueryLog>, String> {
            Ok(None)
        }

        fn projection_state(&self, _session: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(self.boundary.clone())
        }
    }

    /// The tool runtime over one workspace header.
    fn runtime_with(cwd: Option<&str>) -> SessionQueryRuntime {
        let mut header = json!({ "createdAt": 1_000, "isSeeded": false });
        if let Some(cwd) = cwd {
            header["cwd"] = json!(cwd);
        }
        let wire = ToolWire {
            header,
            boundary: None,
        };
        SessionQueryRuntime {
            engine: Arc::new(SessionQueryEngine::new(
                Arc::new(wire),
                EngineConfig::default(),
            )),
            max_search_results: BAKED_MAX_SEARCH_RESULTS,
        }
    }

    #[test]
    fn caller_requires_the_agent_and_resolves_the_boundary() {
        let runtime = runtime_with(Some("/repo"));
        let error = caller_of(None, &runtime).expect_err("no agent");
        assert_eq!(
            error.code.as_deref(),
            Some("SESSION_QUERY_TOOL_MISSING_AGENT")
        );
        assert_eq!(
            error.message,
            "session query tools require an agent-bound caller"
        );
        let caller = caller_of(Some("s1"), &runtime).expect("caller");
        assert_eq!(caller.id, "s1");
        assert!(caller.boundary.is_none());
    }

    #[test]
    fn normalize_query_refuses_blank_and_nul() {
        let error = normalize_query("   ").expect_err("blank");
        assert_eq!(
            error.message,
            "session-search query must contain non-whitespace text"
        );
        let error = normalize_query("bad\u{0}query").expect_err("nul");
        assert_eq!(error.message, "session-search query must not contain NUL");
        assert_eq!(
            normalize_query("  fix   the  lock ").expect("normalized"),
            "fix the lock"
        );
    }

    #[test]
    fn session_search_walks_the_filters_then_hits_the_disabled_state() {
        let runtime = runtime_with(None);
        // The missing workspace refuses before the query normalization.
        let error = execute_session_search(&json!({ "query": "x" }), Some("s1"), &runtime)
            .expect_err("no workspace");
        assert_eq!(
            error.message,
            "cross-session search is unavailable because the caller session has no workspace"
        );

        let runtime = runtime_with(Some("/repo"));
        // The blank query refuses before the provider leg.
        let error = execute_session_search(&json!({ "query": "  " }), Some("s1"), &runtime)
            .expect_err("blank");
        assert_eq!(
            error.message,
            "session-search query must contain non-whitespace text"
        );
        // The parent ids whose corpus authorization answers nothing
        // collapse onto the empty text (the empty array itself refuses
        // the dsh non-empty assert).
        let value = execute_session_search(
            &json!({ "query": "x", "parent_session_ids": ["unknown-parent"] }),
            Some("s1"),
            &runtime,
        )
        .expect("unauthorized parents");
        assert_eq!(value, json!(format_empty_session_search()));
        // The provider leg reaches the disabled terminal state.
        let error = execute_session_search(&json!({ "query": "x" }), Some("s1"), &runtime)
            .expect_err("disabled");
        assert_eq!(error.code.as_deref(), Some("SESSION_QUERY_SEARCH_DISABLED"));
        assert_eq!(
            error.message,
            "session search is disabled in this deployment"
        );
    }

    #[test]
    fn event_search_caps_the_current_session_step() {
        let runtime = runtime_with(Some("/repo"));
        // The absent boundary marker refuses with the dsh text.
        let error = execute_event_search(&json!({ "query": "x" }), Some("s1"), &runtime)
            .expect_err("no boundary");
        assert_eq!(
            error.code.as_deref(),
            Some("SESSION_QUERY_TOOL_NO_CURRENT_STEP")
        );
        assert_eq!(
            error.message,
            "current-session search requires an active step boundary"
        );

        // The null marker caps the range to the degenerate bound the
        // filter validation rejects.
        let wire = ToolWire {
            header: json!({ "createdAt": 1_000, "isSeeded": false, "cwd": "/repo" }),
            boundary: Some(json!({ "lastStepStartSeq": Value::Null })),
        };
        let degenerate = SessionQueryRuntime {
            engine: Arc::new(SessionQueryEngine::new(
                Arc::new(wire),
                EngineConfig::default(),
            )),
            max_search_results: BAKED_MAX_SEARCH_RESULTS,
        };
        let error = execute_event_search(&json!({ "query": "x" }), Some("s1"), &degenerate)
            .expect_err("degenerate");
        assert_eq!(
            error.message,
            "sequence upper bound must be a non-negative safe integer"
        );
    }

    #[test]
    fn event_read_validates_the_sequence_shapes() {
        let runtime = runtime_with(Some("/repo"));
        let error =
            execute_event_read(&json!({ "seq": -1 }), Some("s1"), &runtime).expect_err("negative");
        assert_eq!(error.message, "seq must be a non-negative safe integer");
        let error = execute_event_read(&json!({ "seq": 0, "before": 1.5 }), Some("s1"), &runtime)
            .expect_err("fraction");
        assert_eq!(error.message, "before must be a non-negative safe integer");
    }

    #[test]
    fn call_cards_project_the_five_shapes() {
        let search = present_session_search_call(&json!({ "query": "lock" }));
        assert_eq!(search["kind"], "search");
        assert_eq!(search["title"], "Search prior sessions");
        assert_eq!(search["rawInput"], "lock");
        let event_search = present_event_search_call(&json!({ "query": "e" }));
        assert_eq!(event_search["title"], "Search session events");
        let trace = present_session_trace_call(&json!({}));
        assert_eq!(trace["title"], "Trace current session");
        assert!(trace.get("rawInput").is_none());
        let trace_named = present_session_trace_call(&json!({ "session_id": "s2" }));
        assert_eq!(trace_named["title"], "Trace session s2");
        assert_eq!(trace_named["rawInput"], "s2");
        let event_target = present_event_target_call("Read event", &json!({ "seq": 7 }));
        assert_eq!(event_target["title"], "Read event 7");
        assert_eq!(event_target["rawInput"], json!({ "seq": 7 }));
        let event_target_named =
            present_event_target_call("Trace event", &json!({ "seq": 3, "session_id": "s9" }));
        assert_eq!(
            event_target_named["rawInput"],
            json!({ "session_id": "s9", "seq": 3 })
        );
    }

    #[test]
    fn render_text_wraps_the_string_block() {
        let rendered = render_text(&json!({}), &json!("the text"));
        assert_eq!(rendered, json!([{ "type": "text", "text": "the text" }]));
    }

    #[test]
    fn the_safe_table_swallows_the_config_and_conflict_rows() {
        let config = sanitize_error(&SessionQueryError::new(
            SessionQueryErrorCode::InvalidConfig,
            "session-query: readWindowMax must be a non-negative integer",
        ));
        assert_eq!(config.code.as_deref(), Some("SESSION_QUERY_TOOL_FAILED"));
        assert_eq!(config.message, "session query operation failed");
        let conflict = sanitize_error(&SessionQueryError::new(
            SessionQueryErrorCode::SourceConflict,
            "session source headers conflict for session \"s1\"",
        ));
        assert_eq!(conflict.code.as_deref(), Some("SESSION_QUERY_TOOL_FAILED"));
        let disabled = sanitize_error(&SessionQueryError::new(
            SessionQueryErrorCode::SearchDisabled,
            "session search is disabled in this deployment",
        ));
        assert_eq!(
            disabled.code.as_deref(),
            Some("SESSION_QUERY_SEARCH_DISABLED")
        );
        assert_eq!(
            disabled.message,
            "session search is disabled in this deployment"
        );
    }

    #[test]
    fn timestamp_ranges_parse_the_bounds_and_refuse_the_shapes() {
        let (from, to) = timestamp_range(
            "created_at",
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-02T00:00:00.5001Z"),
        )
        .expect("range");
        assert_eq!(from, Some(1_767_225_600_000.0));
        let upper = to.unwrap();
        assert!(
            upper > 1_767_312_000_500.0 && upper < 1_767_312_000_501.0,
            "the sub-ms remainder steps one ULP below the next millisecond: {upper:?}"
        );
        let error =
            timestamp_range("created_at", Some("2026-13-01T00:00:00Z"), None).expect_err("month");
        assert_eq!(
            error.message,
            "session created_at range from must be a valid ISO 8601 timestamp"
        );
        let error =
            timestamp_range("created_at", Some("2026-01-01 00:00:00Z"), None).expect_err("shape");
        assert_eq!(
            error.message,
            "session created_at range from must be an ISO 8601 timestamp with Z or a numeric offset"
        );
        let error = timestamp_range(
            "time",
            Some("2026-01-02T00:00:00Z"),
            Some("2026-01-01T00:00:00Z"),
        )
        .expect_err("order");
        assert_eq!(
            error.message,
            "session time range from must be less than or equal to to"
        );
    }
}
