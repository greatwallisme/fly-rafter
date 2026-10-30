//! The fr-session service face over the dougong dynamic channel.
//!
//! Phase 13 slice H: the JSON method surface over one assembled
//! [`SessionStore`] + [`JsonlBackend`] runtime, and the Q43-A outbound
//! drain lane the host event pump relays onto the kernel bus.
//!
//! Method map (0/1 are the shell's declare-event-types/describe):
//! - 2 create: `{id?, meta?, seed?, inheritedEventCount?}` — the full
//!   dsh create transaction (prepare, take write ownership, publish) with
//!   the persistence hook at the `createStoredSession` position;
//! - 3 append: `{id, type, data, surfaceOp?, sourceEventSeqs?}` — the dsh
//!   `session.append(type, data, opts?)` contract; time is assigned here
//!   from the injected clock (dsh `Date.now()` is internal too);
//! - 4 flush: `{id}` — the store flush checkpoint; replies
//!   `{"participated": bool}` (dsh flush return);
//! - 5 fork: `{source, childId?, boundary?}` — the store fork rejection
//!   family; the child takes write ownership before publication;
//! - 6 resume: `{id}` — the dsh agent-loop resume flow at session level:
//!   write open first (concurrent-resume exclusion), cold read, crash
//!   repair closers appended through the handle, then publish;
//! - 7 restore: `{id?, meta?, seed, inheritedEventCount?}` — the store
//!   restore primitive (in-memory rebuild + publish, no persistence);
//! - 8 drain: `{cursors?}` — the outbound mirror lane (below);
//! - 9 read: `{id, from?, to?}` — one live session's log slice with the
//!   header and inherited count (Q107-A; the `Session.seq`/`eventAt`
//!   read face over the wire, for consumer-side tail-scan folds).
//!
//! Outbound lane (Q43-A ruling): every store edge (created/event/flush/
//! disposed) lands in a per-session cursor-numbered mirror log; the host
//! event pump drains it and re-emits the records onto the kernel bus as
//! `session/*` events. The lane is advisory (persistence correctness lives
//! entirely in the in-crate synchronous path), single-consumer (the pump),
//! and destructive on drain: everything at or below the caller's per-
//! session cursor is dropped, and the reply carries the next cursors.
//!
//! The live write routing (dsh `session/event` -> `enqueueLive`) gates on
//! publication: a dsh pre-publication append never re-emits through
//! `session/event`, so the same append here stays out of the write buffer —
//! otherwise a vetoed create would drain those events into a materialized
//! artifact and leave residue where the dsh contract promises none.
//!
//! Texts: every refusal below reuses the owning check's message verbatim;
//! the fr-authored texts (structurally unreachable in dsh's in-process
//! shapes) are the plugin-record surface-metadata refusal, the
//! sourceEventSeqs-without-surfaceOp refusal, and the drain cursor domain.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

use fr_wire::event::{SessionEvent, SessionEventData, SurfaceOp};
use fr_wire::ids::SessionId;

use crate::persist::tracker::lock;
use crate::persist::{JsonlBackend, JsonlSessionHandle, PersistError, SessionAccess};
use crate::session::SurfaceIntent;
use crate::store::{ListenerError, SessionMeta, SessionStore};
use crate::vocabulary::EventVocabulary;

/// The provided-service name consumers inject.
///
/// The dsh domain service name; cross-plugin constants live in this
/// shell-free module so dependent artifacts never pull the FFI export
/// object in (the rlib-pulls-by-object link judgment, batch 2a).
pub const SERVICE_NAME: &str = "session-persistence";

/// Method selector: declare durable event type names
/// (`{"types": ["...", ...]}` — the activation-time vocabulary face).
pub const METHOD_DECLARE_EVENT_TYPES: u32 = 0;

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 1;

/// Method selector: create a session (prepare + ownership + publish).
pub const METHOD_SESSION_CREATE: u32 = 2;
/// Method selector: append one typed event.
pub const METHOD_SESSION_APPEND: u32 = 3;
/// Method selector: run the flush checkpoint for one session.
pub const METHOD_SESSION_FLUSH: u32 = 4;
/// Method selector: fork one live session.
pub const METHOD_SESSION_FORK: u32 = 5;
/// Method selector: resume a stored session (write open + repair + publish).
pub const METHOD_SESSION_RESUME: u32 = 6;
/// Method selector: rebuild a session in memory from a seed payload.
pub const METHOD_SESSION_RESTORE: u32 = 7;
/// Method selector: drain the outbound mirror lane.
pub const METHOD_SESSION_DRAIN: u32 = 8;
/// Method selector: read one live session's log slice (`{id, from?, to?}`).
///
/// Q107-A — the dsh `Session.seq`/`eventAt` read face over the wire, so
/// consumers can tail-scan folds without holding the session object.
pub const METHOD_SESSION_READ: u32 = 9;
/// Method selector: read one live session's latest `request/header`
/// snapshot (`{id}` → `{"header": EpochHeader | null}`; the dsh
/// `session.requestHeader()`; the Q16b-1A route-gate seam).
pub const METHOD_REQUEST_HEADER: u32 = 10;

/// Which store edge one mirror record carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorKind {
    Created,
    Event,
    Flush,
    Disposed,
}

impl MirrorKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Event => "event",
            Self::Flush => "flush",
            Self::Disposed => "disposed",
        }
    }
}

/// One outbound mirror record: a store edge, its per-session delivery
/// cursor, and the JSON payload the host pump re-emits.
#[derive(Debug, Clone)]
struct MirrorRecord {
    session: SessionId,
    kind: MirrorKind,
    cursor: u64,
    payload: serde_json::Value,
}

/// The outbound mirror log: per-session delivery cursors plus the global
/// record order. Unbounded by contract — the pump drains promptly, and a
/// backpressured advisory lane must not lose edges (the dsh live-write
/// buffer makes the same trade for persistence).
#[derive(Debug, Default)]
struct OutboundLog {
    cursors: HashMap<SessionId, u64>,
    records: VecDeque<MirrorRecord>,
}

impl OutboundLog {
    fn push(&mut self, session: SessionId, kind: MirrorKind, payload: serde_json::Value) {
        let cursor = self.cursors.entry(session.clone()).or_insert(0);
        *cursor += 1;
        self.records.push_back(MirrorRecord {
            session,
            kind,
            cursor: *cursor,
            payload,
        });
    }

    /// Drops every record at or below the caller's per-session floor and
    /// returns the rest with the next cursors (the highest cursor seen per
    /// session, delivered or dropped).
    fn drain(
        &mut self,
        floors: &HashMap<String, u64>,
    ) -> (
        Vec<MirrorRecord>,
        serde_json::Map<String, serde_json::Value>,
    ) {
        let mut delivered = Vec::new();
        let mut next = serde_json::Map::new();
        while let Some(record) = self.records.pop_front() {
            let floor = floors.get(record.session.as_str()).copied().unwrap_or(0);
            if record.cursor <= floor {
                next.insert(
                    record.session.as_str().to_owned(),
                    serde_json::Value::from(record.cursor),
                );
                continue;
            }
            // Per-session cursors ascend in push order, so the last record
            // seen wins the reply cursor.
            next.insert(
                record.session.as_str().to_owned(),
                serde_json::Value::from(record.cursor),
            );
            delivered.push(record);
        }
        (delivered, next)
    }
}

type Shared<T> = Arc<Mutex<T>>;

/// The assembled session runtime: the publication store, the persistence
/// backend, and the two shared cells its store listeners write through.
pub struct SessionService {
    store: SessionStore,
    backend: Arc<JsonlBackend>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// Sessions past their publication edge — the live-routing gate.
    announced: Shared<HashSet<SessionId>>,
    outbound: Shared<OutboundLog>,
    /// The live persistence handles the service owns: create/fork/resume
    /// retain them here (the mirror machinery routes through the tracker
    /// entries an adopted handle keeps), and the disposal edge removes them.
    handles: Shared<HashMap<SessionId, JsonlSessionHandle>>,
}

impl std::fmt::Debug for SessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionService")
            .field("store", &self.store)
            .field("announced", &lock(&self.announced).len())
            .field("outbound", &lock(&self.outbound).records.len())
            .field("handles", &lock(&self.handles).len())
            .finish_non_exhaustive()
    }
}

impl SessionService {
    /// Assembles the runtime: builds the backend, subscribes the four store
    /// listeners (persistence first — registration order decides which
    /// flush failure the store reports), then the mirror listeners.
    ///
    /// # Errors
    ///
    /// Returns the backend build failure (an unusable root or a failed
    /// scheduler spawn).
    pub fn new(
        root: &Path,
        known: EventVocabulary,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
        warn: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<Self, PersistError> {
        let backend = JsonlBackend::builder(root, known)
            .warn_hook(Arc::clone(&warn))
            .build()?;
        // The struct keeps its own clock clone (append timestamps).
        let mut store = SessionStore::new(Arc::clone(&clock), warn);
        let announced: Shared<HashSet<SessionId>> = Arc::new(Mutex::new(HashSet::new()));
        let outbound: Shared<OutboundLog> = Arc::new(Mutex::new(OutboundLog::default()));
        let handles: Shared<HashMap<SessionId, JsonlSessionHandle>> =
            Arc::new(Mutex::new(HashMap::new()));

        // session/event: live routing (gated on publication) + mirror.
        let backend_event = Arc::clone(&backend);
        let announced_event = Arc::clone(&announced);
        let outbound_event = Arc::clone(&outbound);
        store.subscribe_event(Arc::new(move |id, event| {
            if lock(&announced_event).contains(id) {
                backend_event.route_live(id, event);
            }
            let payload = serde_json::to_value(event)
                .map_err(|error| format!("session mirror encode failed: {error}"))?;
            lock(&outbound_event).push(
                id.clone(),
                MirrorKind::Event,
                serde_json::json!({ "sessionId": id.as_str(), "event": payload }),
            );
            Ok(())
        }));

        // session/flush: persistence drain+flush first (the registered-order
        // first failure is the store's), then the mirror record.
        let backend_flush = Arc::clone(&backend);
        store.subscribe_flush(Arc::new(move |id| {
            backend_flush
                .flush_session(id)
                .map_err(|error| error.to_string())
        }));
        let outbound_flush = Arc::clone(&outbound);
        store.subscribe_flush(Arc::new(move |id| {
            lock(&outbound_flush).push(
                id.clone(),
                MirrorKind::Flush,
                serde_json::json!({ "sessionId": id.as_str() }),
            );
            Ok(())
        }));

        // session/disposed: unmark routing, mirror the edge, then close the
        // write handle (a close failure surfaces as the listener error the
        // store contains — the dsh warn-only disposition).
        let backend_disposed = Arc::clone(&backend);
        let announced_disposed = Arc::clone(&announced);
        let outbound_disposed = Arc::clone(&outbound);
        let handles_disposed = Arc::clone(&handles);
        store.subscribe_disposed(Arc::new(move |id| {
            lock(&announced_disposed).remove(id);
            lock(&handles_disposed).remove(id);
            lock(&outbound_disposed).push(
                id.clone(),
                MirrorKind::Disposed,
                serde_json::json!({ "sessionId": id.as_str() }),
            );
            backend_disposed
                .close_session(id)
                .map_err(|error| error.to_string())
        }));

        // session/created: mark the publication edge and mirror it. The
        // mirror payload carries the id; headers travel the stat/open faces.
        let announced_created = Arc::clone(&announced);
        let outbound_created = Arc::clone(&outbound);
        store.subscribe_created(Arc::new(move |id| {
            lock(&announced_created).insert(id.clone());
            lock(&outbound_created).push(
                id.clone(),
                MirrorKind::Created,
                serde_json::json!({ "sessionId": id.as_str() }),
            );
            Ok(())
        }));

        Ok(Self {
            store,
            backend,
            clock,
            announced,
            outbound,
            handles,
        })
    }

    /// The publication store (read-only face).
    #[must_use]
    pub const fn store(&self) -> &SessionStore {
        &self.store
    }

    /// The persistence backend (stat/list/flush/teardown face).
    #[must_use]
    pub const fn backend(&self) -> &Arc<JsonlBackend> {
        &self.backend
    }

    /// Test access to the listener registries.
    #[cfg(test)]
    pub(crate) const fn store_mut(&mut self) -> &mut SessionStore {
        &mut self.store
    }

    /// The dsh install teardown arm: close every open handle, draining
    /// their live buffers first; failures aggregate.
    ///
    /// # Errors
    ///
    /// Returns the `<name> dispose failed` aggregate.
    pub fn teardown(&self) -> Result<(), PersistError> {
        self.backend.close_all()
    }

    /// Stores the seed a session replayed into the store through its write
    /// handle (dsh `appendUnstoredSuffix` at the publication commit point).
    /// Runs after the publication edge: a veto rolls the session back with
    /// an unmaterialized handle, so the veto path keeps the no-residue
    /// contract the dsh create path documents.
    ///
    /// # Errors
    ///
    /// Returns the append failure text.
    fn sync_unstored_suffix(&self, id: &SessionId) -> Result<(), String> {
        let suffix = self
            .store
            .get(id)
            .map(|session| session.events().to_vec())
            .unwrap_or_default();
        if suffix.is_empty() {
            return Ok(());
        }
        self.backend
            .append_suffix(id, &suffix)
            .map_err(|error| error.to_string())
    }
}

/// The methods' shared request decode helpers. Unknown payload keys are
/// ignored (the fabric envelope is fr-authored; extension stays additive).
fn require_str(request: &serde_json::Value, key: &str) -> Result<String, String> {
    request
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("session service payload requires string {key}"))
}

fn require_id(request: &serde_json::Value, key: &str) -> Result<SessionId, String> {
    require_str(request, key).map(SessionId::new)
}

fn optional_id(request: &serde_json::Value, key: &str) -> Result<Option<SessionId>, String> {
    match request.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|raw| Some(SessionId::new(raw)))
            .ok_or_else(|| format!("session service payload field {key} must be a string")),
    }
}

fn optional_u64(request: &serde_json::Value, key: &str) -> Result<Option<u64>, String> {
    match request.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            format!("session service payload field {key} must be a non-negative safe integer")
        }),
    }
}

fn optional_str(request: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match request.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_owned()))
            .ok_or_else(|| format!("session service payload field {key} must be a string")),
    }
}

fn optional_bool(request: &serde_json::Value, key: &str) -> Result<Option<bool>, String> {
    match request.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("session service payload field {key} must be a boolean")),
    }
}

/// Decodes the shared `meta` object; absent fields keep their defaults, and
/// wrong-typed fields are refused (the same fail-closed decode the numeric
/// and origin fields already apply).
fn decode_meta(request: &serde_json::Value) -> Result<SessionMeta, String> {
    let Some(meta) = request.get("meta") else {
        return Ok(SessionMeta::default());
    };
    let origin = match meta.get("origin") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<fr_wire::event::Origin>(value.clone())
                .map_err(|error| format!("meta.origin must be a wire origin: {error}"))?,
        ),
    };
    Ok(SessionMeta {
        created_at: match meta.get("createdAt") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(
                value
                    .as_i64()
                    .ok_or_else(|| "meta.createdAt must be an integer".to_owned())?,
            ),
        },
        cwd: optional_str(meta, "cwd")?,
        parent_session: optional_id(meta, "parentSession")?,
        is_seeded: optional_bool(meta, "isSeeded")?.unwrap_or(false),
        origin,
        delegation_depth: optional_u64(meta, "delegationDepth")?,
        agent_preset: optional_str(meta, "agentPreset")?,
    })
}

fn decode_seed(request: &serde_json::Value) -> Result<Vec<SessionEvent>, String> {
    match request.get("seed") {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(value) => serde_json::from_value::<Vec<SessionEvent>>(value.clone())
            .map_err(|error| format!("seed events must be wire event envelopes: {error}")),
    }
}

/// Decodes one `session.append` request's raw fields: the dsh
/// `append(type, data, opts?)` contract over the wire. Routing between the
/// plugin-record path and the typed path stays with the caller.
fn decode_append(request: &serde_json::Value) -> Result<AppendParts, String> {
    let type_name = require_str(request, "type")?;
    let data_value = request
        .get("data")
        .cloned()
        .ok_or_else(|| "session service payload requires data".to_owned())?;
    let surface_op = match request.get("surfaceOp") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<SurfaceOp>(value.clone())
                .map_err(|error| format!("surfaceOp must be a wire surface op: {error}"))?,
        ),
    };
    let source_event_seqs = match request.get("sourceEventSeqs") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<Vec<fr_wire::ids::SessionSeq>>(value.clone())
                .map_err(|error| format!("sourceEventSeqs must be a seq array: {error}"))?,
        ),
    };
    Ok((type_name, data_value, surface_op, source_event_seqs))
}

/// One decoded `session.append` request: type key, raw data object, and
/// the optional surface metadata.
type AppendParts = (
    String,
    serde_json::Value,
    Option<SurfaceOp>,
    Option<Vec<fr_wire::ids::SessionSeq>>,
);

fn persistence_text(error: &PersistError) -> ListenerError {
    error.to_string()
}

fn service_create(
    service: &mut SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = optional_id(request, "id")?;
    let meta = decode_meta(request)?;
    let seed = decode_seed(request)?;
    let inherited = optional_u64(request, "inheritedEventCount")?.unwrap_or(0);
    let backend = Arc::clone(&service.backend);
    let handles = Arc::clone(&service.handles);
    let session_id = service
        .store
        .create_with(id, &meta, seed, inherited, |session, inherited| {
            backend
                .create(session.header(), inherited)
                .map(|handle| {
                    lock(&handles).insert(session.header().id.clone(), handle);
                })
                .map_err(|error| persistence_text(&error))
        })
        .map_err(|error| error.to_string())?;
    service.sync_unstored_suffix(&session_id)?;
    Ok(serde_json::json!({ "sessionId": session_id.as_str() }))
}

fn service_append(
    service: &mut SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = require_id(request, "id")?;
    let (type_name, data_value, surface_op, source_event_seqs) = decode_append(request)?;
    let time = (service.clock)();
    if crate::admission::is_plugin_record_type(&type_name) {
        // dsh appendPluginRecord carries no surface parameter; a surface
        // field on the wire has no dsh shape and refuses.
        if surface_op.is_some() || source_event_seqs.is_some() {
            return Err(format!(
                "plugin record \"{type_name}\" cannot carry surface metadata"
            ));
        }
        let serde_json::Value::Object(object) = data_value else {
            return Err("plugin record data must be a JSON object".to_owned());
        };
        service
            .store
            .append_record(&id, time, &type_name, serde_json::Value::Object(object))
            .map_err(|error| error.to_string())?;
    } else {
        let data = SessionEventData::from_parts(&type_name, data_value)?;
        // `sourceEventSeqs` without `surfaceOp` has no dsh runtime shape
        // (the TS overload pair cannot express it) and refuses.
        let intent = match (surface_op, source_event_seqs) {
            (Some(surface_op), source_event_seqs) => Some(SurfaceIntent {
                surface_op,
                source_event_seqs,
            }),
            (None, None) => None,
            (None, Some(_)) => {
                return Err(format!(
                    "session event \"{type_name}\" cannot carry sourceEventSeqs without a surfaceOp"
                ));
            }
        };
        if let Some(intent) = intent {
            service
                .store
                .append_surface(&id, time, data, intent)
                .map_err(|error| error.to_string())?;
        } else {
            service
                .store
                .append_log_only(&id, time, data)
                .map_err(|error| error.to_string())?;
        }
    }
    let committed = service
        .store
        .get(&id)
        .and_then(|session| session.events().last())
        .ok_or_else(|| format!("session \"{id}\" is not live in this store"))
        .and_then(|event| {
            serde_json::to_value(event)
                .map_err(|error| format!("session event encode failed: {error}"))
        })?;
    Ok(serde_json::json!({ "event": committed }))
}

fn service_flush(
    service: &mut SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = require_id(request, "id")?;
    let participated = service
        .store
        .flush(&id)
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "participated": participated }))
}

fn service_fork(
    service: &mut SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let source = require_id(request, "source")?;
    let child_id = optional_id(request, "childId")?;
    let boundary = match request.get("boundary") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .ok_or_else(|| "boundary must be a non-negative safe integer".to_owned())?,
        ),
    };
    let backend = Arc::clone(&service.backend);
    let handles = Arc::clone(&service.handles);
    let session_id = service
        .store
        .fork_with(&source, child_id, boundary, |session, inherited| {
            backend
                .create(session.header(), inherited)
                .map(|handle| {
                    lock(&handles).insert(session.header().id.clone(), handle);
                })
                .map_err(|error| persistence_text(&error))
        })
        .map_err(|error| error.to_string())?;
    service.sync_unstored_suffix(&session_id)?;
    Ok(serde_json::json!({ "sessionId": session_id.as_str() }))
}

fn service_resume(
    service: &mut SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = require_id(request, "id")?;
    // Write ownership FIRST: a live resume holds the claim, so a second
    // resume of the same id refuses before any I/O (dsh resumeWith).
    let handle = service
        .backend
        .open(&id, SessionAccess::Write)
        .map_err(|error| persistence_text(&error))?;
    let outcome = (|| -> Result<SessionId, String> {
        let read = handle
            .read(0, fr_wire::ids::SAFE_INTEGER_MAX)
            .map_err(|error| persistence_text(&error))?;
        // Semantic crash repair: persistence hands back the physically
        // valid log; an interrupted final turn receives its synthetic
        // closers through the same handle as an ordinary batch.
        let closers = crate::repair::interrupted_turn_closers(&read.events)
            .map_err(|error| error.to_string())?;
        if !closers.is_empty() {
            handle
                .append(&closers)
                .map_err(|error| persistence_text(&error))?;
        }
        let mut seed = read.events;
        seed.extend(closers);
        let inherited = handle.inherited_event_count();
        let header = handle.header();
        let meta = SessionMeta {
            created_at: Some(header.created_at),
            cwd: header.cwd.clone(),
            parent_session: header.parent_session.clone(),
            is_seeded: header.is_seeded,
            origin: header.origin,
            delegation_depth: header.delegation_depth,
            agent_preset: header.agent_preset.clone(),
        };
        service
            .store
            .create_with(Some(id.clone()), &meta, seed, inherited, |_, _| Ok(()))
            .map_err(|error| error.to_string())
    })();
    // Rollback swallows the close failure: the setup failure is primary
    // (the veto path already closed the handle through the disposal edge).
    // Success retains the handle: a live resume keeps holding the claim, so
    // a second resume of the same id refuses before any I/O (dsh resumeWith).
    if outcome.is_err() {
        let _ = handle.close();
    } else {
        lock(&service.handles).insert(id, handle);
    }
    let session_id = outcome?;
    Ok(serde_json::json!({ "sessionId": session_id.as_str() }))
}

fn service_restore(
    service: &mut SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = optional_id(request, "id")?;
    let meta = decode_meta(request)?;
    let seed = decode_seed(request)?;
    let inherited = optional_u64(request, "inheritedEventCount")?.unwrap_or(0);
    let session_id = service
        .store
        .create_with(id, &meta, seed, inherited, |_, _| Ok(()))
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "sessionId": session_id.as_str() }))
}

fn service_drain(
    service: &SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let floors: HashMap<String, u64> = match request.get("cursors") {
        None | Some(serde_json::Value::Null) => HashMap::new(),
        Some(value) => {
            let Some(object) = value.as_object() else {
                return Err("cursors must be an object of session id to cursor".to_owned());
            };
            let mut floors = HashMap::with_capacity(object.len());
            for (session, cursor) in object {
                let cursor = cursor.as_u64().ok_or_else(|| {
                    format!(
                        "drain cursor for session \"{session}\" must be a non-negative safe integer"
                    )
                })?;
                floors.insert(session.clone(), cursor);
            }
            floors
        }
    };
    let (records, next) = lock(&service.outbound).drain(&floors);
    let records: Vec<serde_json::Value> = records
        .into_iter()
        .map(|record| {
            serde_json::json!({
                "sessionId": record.session.as_str(),
                "kind": record.kind.as_str(),
                "cursor": record.cursor,
                "payload": record.payload,
            })
        })
        .collect();
    Ok(serde_json::json!({ "records": records, "cursors": next }))
}

fn service_read(
    service: &SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = require_id(request, "id")?;
    let session = service
        .store
        .get(&id)
        .ok_or_else(|| format!("session \"{id}\" is not live in this store"))?;
    let events = session.events();
    let total = events.len();
    let total_u64 = u64::try_from(total).unwrap_or(u64::MAX);
    let from = optional_u64(request, "from")?.unwrap_or(0);
    let to = optional_u64(request, "to")?.unwrap_or(total_u64);
    if from > to || to > total_u64 {
        return Err(format!(
            "session read range {from}..{to} is outside the log (0..{total_u64})"
        ));
    }
    let from_index = usize::try_from(from).unwrap_or(usize::MAX);
    let to_index = usize::try_from(to).unwrap_or(usize::MAX);
    let slice: Vec<serde_json::Value> = events[from_index..to_index]
        .iter()
        .map(|event| {
            serde_json::to_value(event)
                .map_err(|error| format!("session event encode failed: {error}"))
        })
        .collect::<Result<_, _>>()?;
    Ok(serde_json::json!({
        "events": slice,
        "total": total_u64,
        "header": session.header(),
        "inheritedEventCount": session.inherited_event_count(),
    }))
}

/// Serves one live session's latest request-header snapshot (the dsh
/// `session.requestHeader()`; the fold is incremental on the session).
fn service_request_header(
    service: &mut SessionService,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = require_id(request, "id")?;
    let session = service
        .store
        .get_mut(&id)
        .ok_or_else(|| format!("session \"{id}\" is not live in this store"))?;
    let header = session.request_header();
    serde_json::to_value(serde_json::json!({
        "header": header,
    }))
    .map_err(|error| format!("request-header reply encode failed: {error}"))
}

/// Serves one dynamic-channel invocation against the assembled runtime: a
/// pure (JSON-in, JSON-out) function so the face stays testable.
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`: payload
/// decode refusals, the owning store/persistence check verbatim, or the
/// unknown-method text.
pub fn dispatch(
    method: u32,
    payload: &[u8],
    service: &mut SessionService,
) -> Result<Vec<u8>, String> {
    let request: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|error| format!("session service payload must be JSON: {error}"))?;
    let reply = match method {
        METHOD_SESSION_CREATE => service_create(service, &request)?,
        METHOD_SESSION_APPEND => service_append(service, &request)?,
        METHOD_SESSION_FLUSH => service_flush(service, &request)?,
        METHOD_SESSION_FORK => service_fork(service, &request)?,
        METHOD_SESSION_RESUME => service_resume(service, &request)?,
        METHOD_SESSION_RESTORE => service_restore(service, &request)?,
        METHOD_SESSION_DRAIN => service_drain(service, &request)?,
        METHOD_SESSION_READ => service_read(service, &request)?,
        METHOD_REQUEST_HEADER => service_request_header(service, &request)?,
        _ => return Err(format!("unknown session-persistence method {method}")),
    };
    serde_json::to_vec(&reply)
        .map_err(|error| format!("session service reply encode failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: u64 = fr_wire::ids::SAFE_INTEGER_MAX;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "fr-session-service-{tag}-{nanos}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn make_service_at(root: &std::path::Path) -> SessionService {
        let clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| 1_000);
        let warn: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|_| {});
        SessionService::new(root, EventVocabulary::core(), clock, warn).expect("service")
    }

    fn call(
        service: &mut SessionService,
        method: u32,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let reply = dispatch(method, body.to_string().as_bytes(), service)?;
        serde_json::from_slice(&reply).map_err(|error| error.to_string())
    }

    fn user_message(text: &str) -> serde_json::Value {
        serde_json::json!({
            "role": "user", "id": "usr-1",
            "content": [{ "type": "text", "text": text }],
            "source": { "kind": "user" }
        })
    }

    #[test]
    fn create_append_flush_mirror_round_trip() {
        let mut service = make_service_at(&temp_root("round-trip"));
        let reply = call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "s1", "meta": { "cwd": "/repo" } }),
        )
        .expect("create");
        assert_eq!(reply["sessionId"], "s1");

        let appended = call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({
                "id": "s1", "type": "user/message",
                "data": user_message("hi"), "surfaceOp": "append"
            }),
        )
        .expect("append surface");
        assert_eq!(appended["event"]["seq"], 0);
        assert_eq!(appended["event"]["type"], "user/message");
        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": "s1", "type": "turn/start", "data": { "turn": 1 } }),
        )
        .expect("append log-only");

        let flushed = call(
            &mut service,
            METHOD_SESSION_FLUSH,
            &serde_json::json!({ "id": "s1" }),
        )
        .expect("flush");
        assert_eq!(flushed["participated"], true);

        let handle = service
            .backend()
            .open(&SessionId::new("s1"), SessionAccess::Read)
            .expect("read open");
        let read = handle.read(0, ALL).expect("read");
        assert_eq!(read.events.len(), 2);
        assert_eq!(handle.header().cwd.as_deref(), Some("/repo"));

        let drained =
            call(&mut service, METHOD_SESSION_DRAIN, &serde_json::json!({})).expect("drain");
        let kinds: Vec<&str> = drained["records"]
            .as_array()
            .expect("records")
            .iter()
            .map(|record| record["kind"].as_str().expect("kind"))
            .collect();
        assert_eq!(kinds, vec!["created", "event", "event", "flush"]);
        assert_eq!(
            drained["records"][1]["payload"]["event"]["type"],
            "user/message"
        );
        assert_eq!(drained["records"][1]["payload"]["sessionId"], "s1");
        assert_eq!(drained["cursors"]["s1"], 4);

        let again = call(
            &mut service,
            METHOD_SESSION_DRAIN,
            &serde_json::json!({ "cursors": { "s1": 4 } }),
        )
        .expect("second drain");
        assert_eq!(again["records"], serde_json::json!([]));
    }

    #[test]
    fn read_returns_slice_header_and_refuses_out_of_range() {
        let mut service = make_service_at(&temp_root("read"));
        call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "s1", "meta": { "cwd": "/repo" } }),
        )
        .expect("create");
        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": "s1", "type": "turn/start", "data": { "turn": 1 } }),
        )
        .expect("append 0");
        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": "s1", "type": "turn/end", "data": { "turn": 1, "reason": { "kind": "completed" } } }),
        )
        .expect("append 1");

        let full = call(
            &mut service,
            METHOD_SESSION_READ,
            &serde_json::json!({ "id": "s1" }),
        )
        .expect("full read");
        assert_eq!(full["total"], 2);
        assert_eq!(full["inheritedEventCount"], 0);
        assert_eq!(full["header"]["cwd"], "/repo");
        let types: Vec<&str> = full["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|event| event["type"].as_str().expect("type"))
            .collect();
        assert_eq!(types, vec!["turn/start", "turn/end"]);

        let tail = call(
            &mut service,
            METHOD_SESSION_READ,
            &serde_json::json!({ "id": "s1", "from": 1 }),
        )
        .expect("tail read");
        assert_eq!(tail["total"], 2);
        assert_eq!(tail["events"][0]["type"], "turn/end");
        assert_eq!(tail["events"].as_array().expect("events").len(), 1);

        let empty = call(
            &mut service,
            METHOD_SESSION_READ,
            &serde_json::json!({ "id": "s1", "from": 2, "to": 2 }),
        )
        .expect("empty slice");
        assert_eq!(empty["events"], serde_json::json!([]));
        assert_eq!(empty["total"], 2);

        let beyond = call(
            &mut service,
            METHOD_SESSION_READ,
            &serde_json::json!({ "id": "s1", "from": 1, "to": 3 }),
        )
        .expect_err("to beyond total");
        assert_eq!(beyond, "session read range 1..3 is outside the log (0..2)");
        let inverted = call(
            &mut service,
            METHOD_SESSION_READ,
            &serde_json::json!({ "id": "s1", "from": 1, "to": 0 }),
        )
        .expect_err("inverted range");
        assert_eq!(
            inverted,
            "session read range 1..0 is outside the log (0..2)"
        );
        let absent = call(
            &mut service,
            METHOD_SESSION_READ,
            &serde_json::json!({ "id": "nope" }),
        )
        .expect_err("absent session");
        assert_eq!(absent, "session \"nope\" is not live in this store");
    }

    /// The request-header face folds the log's latest header snapshot
    /// (null before the first header) and refuses a non-live session
    /// (the Q16b-1A route-gate seam).
    #[test]
    fn request_header_folds_the_latest_snapshot() {
        let mut service = make_service_at(&temp_root("req-header"));
        call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "s1", "meta": { "cwd": "/repo" } }),
        )
        .expect("creates");
        let id = "s1".to_owned();

        let before = call(
            &mut service,
            METHOD_REQUEST_HEADER,
            &serde_json::json!({ "id": id }),
        )
        .expect("before any request");
        assert_eq!(before["header"], serde_json::Value::Null);

        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": id, "type": "request/header", "data": { "header": { "config": { "provider": "p", "model": "first" } }, "reason": "initial" } }),
        )
        .expect("header 0");
        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": id, "type": "request/header", "data": { "header": { "config": { "provider": "p", "model": "second" } }, "reason": "change" } }),
        )
        .expect("header 1");

        let routed = call(
            &mut service,
            METHOD_REQUEST_HEADER,
            &serde_json::json!({ "id": id }),
        )
        .expect("latest header");
        assert_eq!(routed["header"]["config"]["model"], "second");

        let absent = call(
            &mut service,
            METHOD_REQUEST_HEADER,
            &serde_json::json!({ "id": "nope" }),
        )
        .expect_err("absent session");
        assert_eq!(absent, "session \"nope\" is not live in this store");
    }

    #[test]
    fn veto_rolls_back_without_residue() {
        let mut service = make_service_at(&temp_root("veto"));
        let gate = service.store_mut().subscribe_created(Arc::new(|id| {
            if id.as_str() == "s-v" {
                Err("veto!".to_owned())
            } else {
                Ok(())
            }
        }));
        let error = call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "s-v", "meta": { "cwd": "/repo" } }),
        )
        .expect_err("vetoed");
        assert_eq!(error, "veto!");

        // The created edge fired before the veto; the rollback still pairs
        // the disposal edge (dsh mark-before-emit).
        let drained =
            call(&mut service, METHOD_SESSION_DRAIN, &serde_json::json!({})).expect("drain");
        let kinds: Vec<&str> = drained["records"]
            .as_array()
            .expect("records")
            .iter()
            .map(|record| record["kind"].as_str().expect("kind"))
            .collect();
        assert_eq!(kinds, vec!["created", "disposed"]);

        // No residue: nothing stat-able, nothing pending — the id is free.
        let snapshot = service
            .backend()
            .stat(&SessionId::new("s-v"))
            .expect("stat");
        assert!(snapshot.is_none());
        assert!(service.store().get(&SessionId::new("s-v")).is_none());

        service.store_mut().unsubscribe(gate);
        call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "s-v", "meta": { "cwd": "/repo" } }),
        )
        .expect("same id recreates");
    }

    #[test]
    fn append_rejection_family() {
        let mut service = make_service_at(&temp_root("reject"));
        call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "s1" }),
        )
        .expect("create");
        let error = call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({
                "id": "s1", "type": "user/message", "data": user_message("hi")
            }),
        )
        .expect_err("surface without marker");
        assert!(error.contains("is surface-eligible and requires a surfaceOp marker"));
        let error = call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({
                "id": "s1", "type": "turn/start",
                "data": { "turn": 0 }, "surfaceOp": "append"
            }),
        )
        .expect_err("non-surface with marker");
        assert!(error.contains("is not surface-eligible and cannot carry surfaceOp"));
        let error = call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({
                "id": "s1", "type": "plugin:alpha/beta",
                "data": {}, "surfaceOp": "append"
            }),
        )
        .expect_err("plugin with marker");
        assert!(error.contains("cannot carry surface metadata"));
        let error = call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({
                "id": "s1", "type": "turn/start",
                "data": { "turn": 0 }, "sourceEventSeqs": [0]
            }),
        )
        .expect_err("sources without marker");
        assert!(error.contains("cannot carry sourceEventSeqs without a surfaceOp"));
        let error = call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({
                "id": "s1", "type": "user/message", "data": { "turn": 0 }
            }),
        )
        .expect_err("data shape mismatch");
        assert!(error.contains("user/message"));
    }

    #[test]
    fn plugin_record_round_trip() {
        let mut service = make_service_at(&temp_root("plugin-record"));
        call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "s1", "meta": { "cwd": "/repo" } }),
        )
        .expect("create");
        let reply = call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": "s1", "type": "plugin:alpha/beta", "data": { "k": 1 } }),
        )
        .expect("plugin record");
        assert_eq!(reply["event"]["ignorable"], true);
        call(
            &mut service,
            METHOD_SESSION_FLUSH,
            &serde_json::json!({ "id": "s1" }),
        )
        .expect("flush");
        let handle = service
            .backend()
            .open(&SessionId::new("s1"), SessionAccess::Read)
            .expect("read open");
        let read = handle.read(0, ALL).expect("read");
        assert_eq!(read.events.len(), 1);
        assert_eq!(read.events[0].event_type(), "plugin:alpha/beta");
    }

    #[test]
    fn resume_repairs_interrupted_turn() {
        let root = temp_root("resume");
        let mut service = make_service_at(&root);
        call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "s1", "meta": { "cwd": "/repo" } }),
        )
        .expect("create");
        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": "s1", "type": "turn/start", "data": { "turn": 1 } }),
        )
        .expect("append");
        call(
            &mut service,
            METHOD_SESSION_FLUSH,
            &serde_json::json!({ "id": "s1" }),
        )
        .expect("flush");
        service.teardown().expect("teardown");
        drop(service);

        let mut resumed = make_service_at(&root);
        call(
            &mut resumed,
            METHOD_SESSION_RESUME,
            &serde_json::json!({ "id": "s1" }),
        )
        .expect("resume");
        let live = resumed.store().get(&SessionId::new("s1")).expect("live");
        assert_eq!(live.seq(), 2);
        assert_eq!(
            live.event_at(1).map(SessionEvent::event_type),
            Some("turn/end")
        );

        // A live session refuses a second resume; another claimant refuses
        // while the resumed handle holds the write claim.
        let error = call(
            &mut resumed,
            METHOD_SESSION_RESUME,
            &serde_json::json!({ "id": "s1" }),
        )
        .expect_err("double resume");
        assert!(error.contains("already owned"));
        let mut third = make_service_at(&root);
        let error = call(
            &mut third,
            METHOD_SESSION_RESUME,
            &serde_json::json!({ "id": "s1" }),
        )
        .expect_err("concurrent claim");
        assert!(error.contains("already owned"));
    }

    #[test]
    fn fork_stores_child_and_inherits() {
        let mut service = make_service_at(&temp_root("fork"));
        call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "p", "meta": { "cwd": "/repo" } }),
        )
        .expect("create");
        for body in [
            serde_json::json!({ "id": "p", "type": "turn/start", "data": { "turn": 1 } }),
            serde_json::json!({
                "id": "p", "type": "user/message",
                "data": user_message("hi"), "surfaceOp": "append"
            }),
            serde_json::json!({
                "id": "p", "type": "turn/end",
                "data": { "turn": 1, "reason": { "kind": "completed" } }
            }),
        ] {
            call(&mut service, METHOD_SESSION_APPEND, &body).expect("append");
        }
        call(
            &mut service,
            METHOD_SESSION_FLUSH,
            &serde_json::json!({ "id": "p" }),
        )
        .expect("flush");

        let reply = call(
            &mut service,
            METHOD_SESSION_FORK,
            &serde_json::json!({ "source": "p", "childId": "c1", "boundary": 1 }),
        )
        .expect("fork");
        assert_eq!(reply["sessionId"], "c1");
        // The seed carries the fork protocol: the inherited prefix, the
        // tagged end-seed cut marker, and the forked closer for the open
        // turn the boundary cut (build_fork_seed).
        let child = service.store().get(&SessionId::new("c1")).expect("child");
        let child_types: Vec<&str> = child
            .events()
            .iter()
            .map(SessionEvent::event_type)
            .collect();
        assert_eq!(
            child_types,
            vec!["turn/start", "user/message", "session/end-seed", "turn/end"]
        );
        assert_eq!(child.inherited_event_count(), 2);

        call(
            &mut service,
            METHOD_SESSION_FLUSH,
            &serde_json::json!({ "id": "c1" }),
        )
        .expect("child flush");
        let handle = service
            .backend()
            .open(&SessionId::new("c1"), SessionAccess::Read)
            .expect("child read open");
        assert!(handle.header().is_seeded);
        assert_eq!(
            handle
                .header()
                .parent_session
                .as_ref()
                .map(SessionId::as_str),
            Some("p")
        );
        assert_eq!(handle.inherited_event_count(), 2);
    }

    #[test]
    fn fork_mints_child_when_absent() {
        let mut service = make_service_at(&temp_root("fork-mint"));
        call(
            &mut service,
            METHOD_SESSION_CREATE,
            &serde_json::json!({ "id": "p", "meta": { "cwd": "/repo" } }),
        )
        .expect("create");
        let reply = call(
            &mut service,
            METHOD_SESSION_FORK,
            &serde_json::json!({ "source": "p" }),
        )
        .expect("fork mints");
        let child = SessionId::new(reply["sessionId"].as_str().expect("id"));
        let live = service.store().get(&child).expect("minted child live");
        assert_eq!(live.seq(), 0);
        assert_eq!(
            live.header().parent_session.as_ref().map(SessionId::as_str),
            Some("p")
        );
    }

    #[test]
    fn restore_rebuilds_in_memory_without_persistence() {
        let mut service = make_service_at(&temp_root("restore"));
        // The dsh constructor refuses an inherited cut whose next event is
        // not the tagged marker: the marker sits at the cut index, so the
        // seed leads with the copied event and marks the cut behind it.
        call(
            &mut service,
            METHOD_SESSION_RESTORE,
            &serde_json::json!({
                "id": "r1", "meta": { "cwd": "/repo", "isSeeded": true }, "inheritedEventCount": 1,
                "seed": [
                    { "type": "turn/start", "seq": 0, "time": 5, "data": { "turn": 0 } },
                    { "type": "session/end-seed", "seq": 1, "time": 6, "data": { "inherited": true } }
                ]
            }),
        )
        .expect("restore");
        let live = service.store().get(&SessionId::new("r1")).expect("live");
        assert_eq!(live.seq(), 2);
        assert_eq!(live.inherited_event_count(), 1);
        let snapshot = service.backend().stat(&SessionId::new("r1")).expect("stat");
        assert!(snapshot.is_none());
    }

    #[test]
    fn drain_cursor_floors_are_per_session() {
        let mut service = make_service_at(&temp_root("drain"));
        for id in ["a", "b"] {
            call(
                &mut service,
                METHOD_SESSION_CREATE,
                &serde_json::json!({ "id": id }),
            )
            .expect("create");
        }
        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": "a", "type": "turn/start", "data": { "turn": 1 } }),
        )
        .expect("append a1");
        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": "a", "type": "turn/end", "data": { "turn": 1, "reason": { "kind": "completed" } } }),
        )
        .expect("append a2");
        call(
            &mut service,
            METHOD_SESSION_APPEND,
            &serde_json::json!({ "id": "b", "type": "turn/start", "data": { "turn": 1 } }),
        )
        .expect("append b1");

        // a's floor 2 drops a's created(1) and first event(2) but keeps its
        // second event(3); b's lane is untouched.
        let drained = call(
            &mut service,
            METHOD_SESSION_DRAIN,
            &serde_json::json!({ "cursors": { "a": 2 } }),
        )
        .expect("drain");
        let seen: Vec<(String, &str, u64)> = drained["records"]
            .as_array()
            .expect("records")
            .iter()
            .map(|record| {
                (
                    record["sessionId"].as_str().expect("id").to_owned(),
                    record["kind"].as_str().expect("kind"),
                    record["cursor"].as_u64().expect("cursor"),
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                ("b".to_owned(), "created", 1),
                ("a".to_owned(), "event", 3),
                ("b".to_owned(), "event", 2),
            ]
        );
        assert_eq!(drained["cursors"]["a"], 3);
        assert_eq!(drained["cursors"]["b"], 2);

        let emptied = call(
            &mut service,
            METHOD_SESSION_DRAIN,
            &serde_json::json!({ "cursors": { "a": 3, "b": 2 } }),
        )
        .expect("second drain");
        assert_eq!(emptied["records"], serde_json::json!([]));
    }

    #[test]
    fn unknown_methods_and_malformed_payloads_fail_with_text() {
        let mut service = make_service_at(&temp_root("unknown"));
        let error = dispatch(99, b"{}", &mut service).expect_err("unknown method");
        assert!(error.contains("unknown session-persistence method 99"));
        let error =
            dispatch(METHOD_SESSION_FLUSH, b"not json", &mut service).expect_err("bad json");
        assert!(error.contains("session service payload must be JSON"));
    }
    #[test]
    fn service_face_constants_match_the_wire_table() {
        use super::{
            METHOD_DECLARE_EVENT_TYPES, METHOD_DESCRIBE, METHOD_SESSION_APPEND,
            METHOD_SESSION_CREATE, METHOD_SESSION_DRAIN, METHOD_SESSION_FLUSH, METHOD_SESSION_FORK,
            METHOD_SESSION_READ, METHOD_SESSION_RESTORE, METHOD_SESSION_RESUME, SERVICE_NAME,
        };
        use fr_wire::services::session_persistence as wire;
        assert_eq!(SERVICE_NAME, wire::SERVICE_NAME);
        assert_eq!(METHOD_DECLARE_EVENT_TYPES, wire::METHOD_DECLARE_EVENT_TYPES);
        assert_eq!(METHOD_DESCRIBE, wire::METHOD_DESCRIBE);
        assert_eq!(METHOD_SESSION_CREATE, wire::METHOD_SESSION_CREATE);
        assert_eq!(METHOD_SESSION_APPEND, wire::METHOD_SESSION_APPEND);
        assert_eq!(METHOD_SESSION_FLUSH, wire::METHOD_SESSION_FLUSH);
        assert_eq!(METHOD_SESSION_FORK, wire::METHOD_SESSION_FORK);
        assert_eq!(METHOD_SESSION_RESUME, wire::METHOD_SESSION_RESUME);
        assert_eq!(METHOD_SESSION_RESTORE, wire::METHOD_SESSION_RESTORE);
        assert_eq!(METHOD_SESSION_DRAIN, wire::METHOD_SESSION_DRAIN);
        assert_eq!(METHOD_SESSION_READ, wire::METHOD_SESSION_READ);
    }
}
