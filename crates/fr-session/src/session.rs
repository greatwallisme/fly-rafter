//! The in-memory session: append-only log, incremental surface fold, the
//! derived-message cache, and the request-header fold (dsh core/session
//! index.ts Session semantics on the typed fr-wire event model).
//!
//! Append-time reentrancy is impossible by construction: `&mut self` serializes
//! every mutation. The event timestamp is an explicit parameter (the clock
//! seam; the host supplies wall time).

#![forbid(unsafe_code)]
use crate::admission::{is_plugin_record_type, validate_event_data};
use crate::error::SessionError;
use crate::surface::{SurfaceFoldResult, apply_plan, plan_event};
use fr_wire::content::TrueOnly;
use fr_wire::event::{EpochHeader, SessionEvent, SessionEventData, SurfaceOp, UnknownEventData};
use fr_wire::ids::SessionSeq;
use fr_wire::message::Message;

/// How a surface-eligible event joins the ordered surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceIntent {
    /// How the event joins the surface.
    pub surface_op: SurfaceOp,
    /// Complete set of earlier source-event seqs this event cites.
    pub source_event_seqs: Option<Vec<SessionSeq>>,
}

/// The derived-message cache: projecting surface nodes plus the content
/// generation it was built under.
#[derive(Debug, Default)]
struct DerivedCache {
    generation: u64,
    projecting: Vec<SessionSeq>,
}

/// An append-only, event-sourced session over typed events.
#[derive(Debug)]
pub struct Session {
    header: fr_wire::event::SessionHeader,
    log: Vec<SessionEvent>,
    surface: SurfaceFoldResult,
    derived: DerivedCache,
    header_fold: Option<EpochHeader>,
    header_fold_seq: usize,
    inherited_event_count: u64,
}

impl Session {
    /// Creates an empty session with the supplied header.
    #[must_use]
    pub fn new(header: fr_wire::event::SessionHeader) -> Self {
        Self {
            header,
            log: Vec::new(),
            surface: SurfaceFoldResult::default(),
            derived: DerivedCache::default(),
            header_fold: None,
            header_fold_seq: 0,
            inherited_event_count: 0,
        }
    }

    /// Replays a seed (replay or fork history) into a session, validating
    /// every event to the same invariants `append` enforces (dsh constructor
    /// seed contract: contiguity from zero, admission, surface fold).
    ///
    /// # Errors
    ///
    /// Returns the owning check's message when a seed event is
    /// non-contiguous, fails admission, or violates the surface fold.
    pub fn from_seed(
        header: fr_wire::event::SessionHeader,
        seed: Vec<SessionEvent>,
    ) -> Result<Self, SessionError> {
        Self::from_seed_parts(header, seed, 0)
    }

    /// Like [`Self::from_seed`], also recording the fork-inherited prefix
    /// length (dsh `inheritedEventCount`: the tagged end-seed seq of a fork
    /// seed; zero for fresh or fully-restored sessions).
    ///
    /// # Errors
    ///
    /// Same contract as [`Self::from_seed`].
    pub fn from_seed_parts(
        header: fr_wire::event::SessionHeader,
        seed: Vec<SessionEvent>,
        inherited_event_count: u64,
    ) -> Result<Self, SessionError> {
        let mut session = Self::new(header);
        session.inherited_event_count = inherited_event_count;
        for event in seed {
            session.append_seed_event(event)?;
        }
        Ok(session)
    }

    fn append_seed_event(&mut self, event: SessionEvent) -> Result<(), SessionError> {
        let expected = self.log.len() as u64;
        if event.seq.get() != expected {
            return Err(SessionError::Other(format!(
                "seed event at index {expected} has seq {} (expected {expected}); seed must be contiguous from 0",
                event.seq.get()
            )));
        }
        validate_event_data(&event)?;
        self.fold_in(&event)?;
        self.log.push(event);
        Ok(())
    }

    /// Folds one already-admitted event into the incremental surface state
    /// (validate-then-apply; a failed plan never mutates the fold).
    fn fold_in(&mut self, event: &SessionEvent) -> Result<(), SessionError> {
        let plan = plan_event(&self.surface, event, &self.log)?;
        if let Some(replacement) = apply_plan(&mut self.surface, event, plan) {
            self.surface.replacements.push(replacement);
        }
        Ok(())
    }
}

impl Session {
    /// Appends one surface-eligible event with its surface intent.
    ///
    /// # Errors
    ///
    /// Returns the owning check's message when the payload is not
    /// surface-eligible, fails admission, or violates the surface fold.
    pub fn append_surface(
        &mut self,
        time: i64,
        data: SessionEventData,
        intent: SurfaceIntent,
    ) -> Result<(), SessionError> {
        let event = SessionEvent {
            seq: SessionSeq::new(self.log.len() as u64)
                .map_err(|e| SessionError::Other(e.to_string()))?,
            time,
            data,
            ignorable: None,
            surface_op: Some(intent.surface_op),
            source_event_seqs: intent.source_event_seqs,
        };
        if !crate::surface::is_surface_eligible(&event) {
            return Err(SessionError::Other(format!(
                "session event \"{}\" is not surface-eligible and cannot carry surfaceOp",
                event.event_type()
            )));
        }
        validate_event_data(&event)?;
        self.fold_in(&event)?;
        self.log.push(event);
        Ok(())
    }

    /// Appends one log-only event (turn/step boundaries, attempts,
    /// `request/header`, and the like). Log-only events carry no surface
    /// metadata.
    ///
    /// # Errors
    ///
    /// Returns the owning check's message when the payload fails admission
    /// or the surface fold rejects the event's metadata.
    pub fn append_log_only(
        &mut self,
        time: i64,
        data: SessionEventData,
    ) -> Result<(), SessionError> {
        let event = SessionEvent {
            seq: SessionSeq::new(self.log.len() as u64)
                .map_err(|e| SessionError::Other(e.to_string()))?,
            time,
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        if crate::surface::is_surface_eligible(&event) {
            return Err(SessionError::Other(format!(
                "session event \"{}\" is surface-eligible and requires a surfaceOp marker",
                event.event_type()
            )));
        }
        validate_event_data(&event)?;
        self.fold_in(&event)?;
        self.log.push(event);
        Ok(())
    }

    /// Commits one experimental plugin record: an event marked `ignorable`
    /// whose type follows the `plugin:` grammar (PRD 7.1, Q33A). A reader
    /// that does not recognize the type retains and skips the record; it
    /// never enters the model-visible surface.
    ///
    /// # Errors
    ///
    /// Returns an error when the type is outside the plugin-record grammar
    /// or the payload is not valid JSON (it arrives pre-parsed here).
    pub fn append_record(
        &mut self,
        time: i64,
        type_name: &str,
        data: serde_json::Value,
    ) -> Result<(), SessionError> {
        if !is_plugin_record_type(type_name) {
            return Err(SessionError::Other(format!(
                "plugin record type \"{type_name}\" must be \"plugin:\" followed by lowercase slash-separated segments"
            )));
        }
        let serde_json::Value::Object(object) = data else {
            return Err(SessionError::Other(
                "plugin record data must be a JSON object".to_owned(),
            ));
        };
        let event = SessionEvent {
            seq: SessionSeq::new(self.log.len() as u64)
                .map_err(|e| SessionError::Other(e.to_string()))?,
            time,
            data: SessionEventData::Unknown(UnknownEventData {
                type_name: type_name.to_owned(),
                data: object,
            }),
            ignorable: Some(TrueOnly),
            surface_op: None,
            source_event_seqs: None,
        };
        self.fold_in(&event)?;
        self.log.push(event);
        Ok(())
    }
}

impl Session {
    /// The event count; also the next event's sequence number.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.log.len() as u64
    }

    /// The immutable event stored at one exact sequence number.
    #[must_use]
    pub fn event_at(&self, seq: u64) -> Option<&SessionEvent> {
        self.log.get(usize::try_from(seq).unwrap_or(usize::MAX))
    }

    /// The full committed log, in order.
    #[must_use]
    pub fn events(&self) -> &[SessionEvent] {
        &self.log
    }

    /// The session's durable header.
    #[must_use]
    pub const fn header(&self) -> &fr_wire::event::SessionHeader {
        &self.header
    }

    /// The current surface event sequences in model-visible order.
    #[must_use]
    pub fn surface_nodes(&self) -> &[SessionSeq] {
        &self.surface.nodes
    }

    /// The number of leading events inherited from a fork parent.
    #[must_use]
    pub const fn inherited_event_count(&self) -> u64 {
        self.inherited_event_count
    }

    /// Derives the LLM message history by walking the ordered surface.
    ///
    /// Cached: a pure tail growth costs only the new nodes; a replacement
    /// (content-generation change) rebuilds the projecting list from every
    /// node. Empty-content system/developer/assistant nodes project to no
    /// message; an empty assistant (a max-tokens step's usage carrier) must
    /// not inject a content-less turn into the provider transcript.
    #[must_use]
    pub fn derive_messages(&mut self) -> Vec<&Message> {
        if self.surface.content_generation != self.derived.generation {
            self.derived.projecting.clear();
            self.derived.generation = self.surface.content_generation;
        }
        let start = self.derived.projecting.len();
        for &seq_value in self.surface.nodes.get(start..).unwrap_or_default() {
            if self.projects_message(seq_value) {
                self.derived.projecting.push(seq_value);
            }
        }
        self.derived
            .projecting
            .iter()
            .filter_map(|node| {
                let event = self
                    .log
                    .get(usize::try_from(node.get()).unwrap_or(usize::MAX))?;
                projected_message(event)
            })
            .collect()
    }

    fn projects_message(&self, seq_value: SessionSeq) -> bool {
        self.log
            .get(usize::try_from(seq_value.get()).unwrap_or(usize::MAX))
            .and_then(projected_message)
            .is_some()
    }

    /// The [`EpochHeader`] in force after the log's last header event, or
    /// `None` before the first `request/header`. Incremental: each header
    /// event folds once (last snapshot wins).
    pub fn request_header(&mut self) -> Option<EpochHeader> {
        if self.header_fold_seq < self.log.len() {
            for event in &self.log[self.header_fold_seq..] {
                if let SessionEventData::RequestHeader(data) = &event.data {
                    self.header_fold = Some(canonical_header(&data.header));
                }
            }
            self.header_fold_seq = self.log.len();
        }
        self.header_fold.clone()
    }
}

/// Projects one event to the message it contributes to derived history, or
/// `None` when it produces none.
fn projected_message(event: &SessionEvent) -> Option<&Message> {
    match &event.data {
        SessionEventData::UserMessage(message) => Some(message),
        SessionEventData::SystemMessage(data) => non_empty(&data.message),
        SessionEventData::DeveloperMessage(data) => non_empty(&data.message),
        SessionEventData::AssistantMessage(data) => non_empty(&data.message),
        SessionEventData::ToolResult(data) => Some(&data.message),
        _ => None,
    }
}

fn non_empty(message: &Message) -> Option<&Message> {
    let known = match message {
        Message::Known(known) => known,
        Message::Other(_) => return Some(message),
    };
    if known.content().is_empty() {
        None
    } else {
        Some(message)
    }
}

/// Normalizes a header to canonical form: empty tool lists and empty
/// adapter-default markers become absent fields.
#[must_use]
pub fn canonical_header(header: &EpochHeader) -> EpochHeader {
    let adapter_defaults = header
        .adapter_defaults
        .filter(|defaults| defaults.reasoning_effort.is_some() || defaults.max_tokens.is_some());
    let tools = header.tools.as_ref().and_then(|tools| {
        if tools.is_empty() {
            None
        } else {
            Some(tools.clone())
        }
    });
    EpochHeader {
        config: header.config.clone(),
        adapter_defaults,
        tools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::seq;
    use crate::{EventVocabulary, refuse_unknown_required};
    use fr_wire::call_config::CallConfig;
    use fr_wire::call_config::CallControls;
    use fr_wire::content::{ContentBlock, KnownContentBlock, RawObject};
    use fr_wire::event::{AppendOp, RequestHeaderData, RequestHeaderReason, TurnStartData};
    use fr_wire::ids::MessageId;
    use fr_wire::message::{KnownMessage, KnownMessageSource, MessageSource};

    fn header() -> fr_wire::event::SessionHeader {
        fr_wire::event::SessionHeader {
            version: 4,
            id: fr_wire::ids::SessionId::new("s-1"),
            created_at: 1,
            cwd: None,
            parent_session: None,
            is_seeded: false,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    fn user_data(text: &str) -> SessionEventData {
        SessionEventData::UserMessage(Message::Known(KnownMessage::User {
            id: MessageId::new("m-1"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
        }))
    }

    fn append_intent() -> SurfaceIntent {
        SurfaceIntent {
            surface_op: SurfaceOp::Append(AppendOp),
            source_event_seqs: None,
        }
    }

    fn turn_start() -> SessionEventData {
        SessionEventData::TurnStart(TurnStartData { turn: 0 })
    }

    #[test]
    fn append_surface_and_log_only_build_nodes_and_skip_log_only() {
        let mut session = Session::new(header());
        session
            .append_log_only(1, turn_start())
            .unwrap_or_else(|e| panic!("log only: {e}"));
        session
            .append_surface(2, user_data("hello"), append_intent())
            .unwrap_or_else(|e| panic!("surface: {e}"));
        assert_eq!(session.seq(), 2);
        assert_eq!(session.surface_nodes(), &[seq(1)]);
    }

    #[test]
    fn derive_messages_skips_empty_and_log_only_and_caches_tail_growth() {
        let mut session = Session::new(header());
        session
            .append_log_only(1, turn_start())
            .unwrap_or_else(|e| panic!("t: {e}"));
        session
            .append_surface(2, user_data("hello"), append_intent())
            .unwrap_or_else(|e| panic!("u: {e}"));
        let first = session.derive_messages();
        assert_eq!(first.len(), 1);
        // Pure tail growth extends the projecting list without a rebuild.
        session
            .append_surface(3, user_data("again"), append_intent())
            .unwrap_or_else(|e| panic!("u2: {e}"));
        let second = session.derive_messages();
        assert_eq!(second.len(), 2);
        // Same state again: the cached projection replays identically.
        let snapshot: Vec<Message> = second.iter().map(|message| (*message).clone()).collect();
        let again = session.derive_messages();
        let again_owned: Vec<Message> = again.iter().map(|message| (*message).clone()).collect();
        assert_eq!(again_owned, snapshot);
    }

    #[test]
    fn empty_assistant_projects_to_no_message() {
        let mut session = Session::new(header());
        let empty_assistant =
            SessionEventData::AssistantMessage(fr_wire::event::AssistantMessageData {
                turn: 0,
                step: 0,
                message: Message::Known(KnownMessage::Assistant {
                    id: MessageId::new("a-1"),
                    content: vec![],
                    source: MessageSource::Known(KnownMessageSource::Model {
                        provider: "p".to_owned(),
                        model: "m".to_owned(),
                        replay_state: None,
                        extra: RawObject::new(),
                    }),
                }),
                stream: vec![],
                usage: Some(fr_wire::call_config::TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    ..fr_wire::call_config::TokenUsage::default()
                }),
                interrupted: None,
            });
        session
            .append_surface(1, empty_assistant, append_intent())
            .unwrap_or_else(|e| panic!("a: {e}"));
        assert_eq!(session.surface_nodes().len(), 1);
        let derived: Vec<&Message> = session.derive_messages();
        assert_eq!(derived.as_slice(), &[] as &[&Message]);
    }

    #[test]
    fn replacement_bumps_generations_and_derive_rebuilds() {
        let mut session = Session::new(header());
        session
            .append_surface(1, user_data("a"), append_intent())
            .unwrap_or_else(|e| panic!("u1: {e}"));
        session
            .append_surface(2, user_data("b"), append_intent())
            .unwrap_or_else(|e| panic!("u2: {e}"));
        let before = session.derive_messages();
        assert_eq!(before.len(), 2);
        let mut replacement = SessionEventData::UserMessage(Message::Known(KnownMessage::User {
            id: MessageId::new("m-3"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "compacted".to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
        }));
        let _ = &mut replacement;
        session
            .append_surface(
                3,
                replacement,
                SurfaceIntent {
                    surface_op: SurfaceOp::Replace(fr_wire::event::ReplaceOp {
                        op: fr_wire::event::ReplaceTag,
                        start_seq: seq(0),
                        end_seq: seq(1),
                    }),
                    source_event_seqs: Some(vec![seq(0), seq(1)]),
                },
            )
            .unwrap_or_else(|e| panic!("r: {e}"));
        assert_eq!(session.surface_nodes(), &[seq(2)]);
        let after = session.derive_messages();
        assert_eq!(after.len(), 1);
    }

    #[test]
    fn request_header_fold_last_wins_and_canonicalizes() {
        let mut session = Session::new(header());
        assert!(session.request_header().is_none());
        session
            .append_log_only(
                1,
                SessionEventData::RequestHeader(RequestHeaderData {
                    header: EpochHeader {
                        config: CallConfig {
                            provider: "p".to_owned(),
                            model: "m".to_owned(),
                            controls: CallControls::default(),
                        },
                        adapter_defaults: Some(fr_wire::call_config::AdapterDefaults::default()),
                        tools: Some(Vec::new()),
                    },
                    reason: RequestHeaderReason::Initial,
                    starts_series: None,
                }),
            )
            .unwrap_or_else(|e| panic!("h1: {e}"));
        let folded = session.request_header().unwrap_or_else(|| panic!("folded"));
        assert!(folded.adapter_defaults.is_none());
        assert!(folded.tools.is_none());
    }

    #[test]
    fn plugin_record_grammar_and_retention() {
        let mut session = Session::new(header());
        session
            .append_record(1, "plugin:example/state", serde_json::json!({"k": "v"}))
            .unwrap_or_else(|e| panic!("record: {e}"));
        let event = session.event_at(0).unwrap_or_else(|| panic!("present"));
        assert!(event.ignorable.is_some());
        match &event.data {
            SessionEventData::Unknown(unknown) => {
                assert_eq!(unknown.type_name, "plugin:example/state");
                assert_eq!(unknown.data.get("k"), Some(&serde_json::json!("v")));
            }
            _ => panic!("record stays unknown"),
        }
        // Plugin records never join the ordered surface.
        assert_eq!(session.surface_nodes(), []);
        let err = session
            .append_record(2, "example/state", serde_json::json!({}))
            .expect_err("bad grammar");
        assert!(err.to_string().contains("must be \"plugin:\""));
    }

    #[test]
    fn vocabulary_declares_and_read_path_refuses_unknown_required() {
        let mut vocabulary = EventVocabulary::core();
        vocabulary
            .declare(&["approval/asked".to_owned()])
            .unwrap_or_else(|e| panic!("declare: {e}"));
        let dup = vocabulary
            .declare(&["approval/asked".to_owned()])
            .expect_err("duplicate");
        assert!(dup.to_string().contains("already declared"));
        let unknown = envelope_of("plugin:example/state", true, 0);
        let required = envelope_of("brand:new-thing", false, 1);
        let events = vec![unknown, required];
        let err = refuse_unknown_required(&events, &vocabulary).expect_err("refusal");
        assert!(
            err.to_string()
                .contains("format v4 contains unknown event type \"brand:new-thing\" at seq 1"),
            "got: {err}"
        );
    }

    fn envelope_of(type_name: &str, ignorable: bool, n: u64) -> SessionEvent {
        SessionEvent {
            seq: seq(n),
            time: 1,
            data: SessionEventData::Unknown(UnknownEventData {
                type_name: type_name.to_owned(),
                data: RawObject::new(),
            }),
            ignorable: ignorable.then_some(TrueOnly),
            surface_op: None,
            source_event_seqs: None,
        }
    }

    #[test]
    fn surface_append_of_log_only_data_is_rejected() {
        let mut session = Session::new(header());
        let err = session
            .append_surface(1, turn_start(), append_intent())
            .expect_err("surface on log-only");
        assert!(err.to_string().contains("is not surface-eligible"));
    }

    #[test]
    fn seed_replays_contiguously_and_rejects_gaps() {
        let mut seed_session = Session::new(header());
        seed_session
            .append_log_only(1, turn_start())
            .unwrap_or_else(|e| panic!("t: {e}"));
        seed_session
            .append_surface(2, user_data("hello"), append_intent())
            .unwrap_or_else(|e| panic!("u: {e}"));
        let seed: Vec<SessionEvent> = seed_session.events().to_vec();
        let replayed =
            Session::from_seed(header(), seed.clone()).unwrap_or_else(|e| panic!("seed: {e}"));
        assert_eq!(replayed.seq(), 2);
        assert_eq!(replayed.surface_nodes(), &[seq(1)]);
        let mut gapped = seed;
        if let Some(last) = gapped.last_mut() {
            last.seq = seq(5);
        }
        let err = Session::from_seed(header(), gapped).expect_err("gap");
        assert!(err.to_string().contains("seed must be contiguous from 0"));
    }

    use fr_wire::event::{
        AssistantAttemptData, AssistantMessageData, DeveloperMessageData, EndSeedData,
        RequestContextData, RequestContextHolder, StepBoundsData, SystemMessageData, ToolCallData,
        ToolResultData, TurnEndData, TurnEndReason, TurnEndReasonKind,
    };
    use fr_wire::ids::ToolCallId;

    /// Shared disk harness for the 回环 facet: materialize one event as its
    /// own session log, read it back through the stored-log gate, and
    /// require deep equality plus a byte-identical wire row.
    struct TempLog(std::path::PathBuf);

    impl TempLog {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-session-type-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_nanos())
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempLog {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn round_trips_on_disk(
        tag: &str,
        seeded: bool,
        inherited: u64,
        events: &[SessionEvent],
        pinned: usize,
    ) {
        let root = TempLog::new(tag);
        let id = fr_wire::ids::SessionId::new("disk-round-trip");
        let mut meta = header();
        meta.id = id.clone();
        meta.is_seeded = seeded;
        let compression = crate::persist::JsonlCompression::None;
        crate::persist::io::materialize(root.path(), &meta, inherited, events, compression)
            .unwrap_or_else(|error| panic!("materialize: {error}"));
        let path =
            crate::persist::filename::log_path(root.path(), None, &id, compression).expect("path");
        let outcome = crate::persist::read::read_stored_log(
            root.path(),
            &path,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .unwrap_or_else(|error| panic!("read: {error}"));
        assert_eq!(outcome.events, events);
        let raw = std::fs::read(&path).expect("raw bytes");
        let text = String::from_utf8(raw).expect("utf-8 log");
        let row = text.lines().nth(pinned + 1).expect("event row");
        assert_eq!(
            row,
            serde_json::to_string(&events[pinned]).unwrap_or_default(),
            "wire row must be byte-identical"
        );
    }

    /// The minimal legal carrier the scenario Given describes: a session
    /// containing the pinned event. Lifecycle-bound types need their
    /// turn/step brackets; the carrier selects the shape.
    fn bracketed(event: SessionEvent, carrier: Carrier) {
        let tag = event.event_type().replace('/', "-");
        let mut events: Vec<SessionEvent> = Vec::new();
        let push = |events: &mut Vec<SessionEvent>, seq_value: u64, data: SessionEventData| {
            events.push(SessionEvent {
                seq: seq(seq_value),
                time: i64::try_from(seq_value + 1).expect("seq fits i64"),
                data,
                ignorable: None,
                surface_op: None,
                source_event_seqs: None,
            });
        };
        match carrier {
            Carrier::Bare => events.push(event),
            Carrier::Turn | Carrier::StepOpen => {
                push(
                    &mut events,
                    0,
                    SessionEventData::TurnStart(TurnStartData { turn: 1 }),
                );
                events.push(event);
            }
            Carrier::Step => {
                push(
                    &mut events,
                    0,
                    SessionEventData::TurnStart(TurnStartData { turn: 1 }),
                );
                push(
                    &mut events,
                    1,
                    SessionEventData::StepStart(StepBoundsData { turn: 1, step: 1 }),
                );
                events.push(event);
            }
            Carrier::Full | Carrier::ToolCall | Carrier::ToolResult => {
                push(
                    &mut events,
                    0,
                    SessionEventData::TurnStart(TurnStartData { turn: 1 }),
                );
                push(
                    &mut events,
                    1,
                    SessionEventData::StepStart(StepBoundsData { turn: 1, step: 1 }),
                );
                if carrier != Carrier::Full {
                    events.push(advertised_assistant(2, 1, 1, "c-1"));
                }
                if carrier == Carrier::ToolCall {
                    events.push(event);
                    events.push(settling_tool_result(4, 1, 1, "c-1"));
                } else if carrier == Carrier::ToolResult {
                    events.push(simple_tool_call(3, 1, 1, "c-1"));
                    events.push(event);
                } else {
                    events.push(event);
                }
                let step_seq: u64 = if carrier == Carrier::Full { 3 } else { 5 };
                push(
                    &mut events,
                    step_seq,
                    SessionEventData::StepEnd(StepBoundsData { turn: 1, step: 1 }),
                );
                push(
                    &mut events,
                    step_seq + 1,
                    SessionEventData::TurnEnd(TurnEndData {
                        turn: 1,
                        reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
                    }),
                );
            }
        }
        let pinned = match carrier {
            Carrier::Bare => 0,
            Carrier::Turn | Carrier::StepOpen => 1,
            Carrier::Step | Carrier::Full => 2,
            Carrier::ToolCall => 3,
            Carrier::ToolResult => 4,
        };
        // The pinned fixture lands at its chain slot; renumber it so the
        // carrier stays contiguous.
        events[pinned].seq = seq(pinned as u64);
        round_trips_on_disk(&tag, false, 0, &events, pinned);
    }

    /// Carrier shapes for [`bracketed`]: `Bare` (self-bracketing types),
    /// `Turn` (open-turn tail), `StepOpen` (open-step tail), `Step` (closed
    /// step inside an open turn), `Full` (closed turn), and the two full
    /// tool-lifecycle chains.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Carrier {
        Bare,
        Turn,
        StepOpen,
        Step,
        Full,
        ToolCall,
        ToolResult,
    }

    /// The dsh 键序锚点 facet: the wire data object's key set equals exactly
    /// the field set the dsh `SessionEventMap` declares for the type
    /// (camelCase preserved; optional fields absent in the fixture stay
    /// absent).
    fn assert_dsh_data_keys(event: &SessionEvent, expected: &[&str]) {
        let value =
            serde_json::to_value(event).unwrap_or_else(|error| panic!("serialize: {error}"));
        assert_eq!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some(event.event_type())
        );
        let data = value
            .get("data")
            .and_then(serde_json::Value::as_object)
            .expect("data object");
        let mut actual: Vec<&str> = data.keys().map(String::as_str).collect();
        actual.sort_unstable();
        let mut expected: Vec<&str> = expected.to_vec();
        expected.sort_unstable();
        assert_eq!(actual, expected, "wire data keys of {}", event.event_type());
    }

    /// Both surface-attribution directions for one log-only type: a
    /// surfaceOp marker is refused and the log-only append stays off the
    /// fold.
    fn assert_log_only_role(data: &SessionEventData) {
        let mut session = Session::new(header());
        let refused = session
            .append_surface(1, data.clone(), append_intent())
            .expect_err("surface on log-only type");
        assert!(
            refused
                .to_string()
                .contains("is not surface-eligible and cannot carry surfaceOp")
        );
        session
            .append_log_only(1, data.clone())
            .unwrap_or_else(|error| panic!("log-only: {error}"));
        assert_eq!(session.surface_nodes(), []);
    }

    /// Both surface-attribution directions for one surface-eligible type: a
    /// log-only append is refused and the surface append joins the fold.
    fn assert_surface_role(data: &SessionEventData) {
        let mut session = Session::new(header());
        let refused = session
            .append_log_only(1, data.clone())
            .expect_err("log-only on surface type");
        assert!(
            refused
                .to_string()
                .contains("is surface-eligible and requires a surfaceOp marker")
        );
        session
            .append_surface(1, data.clone(), append_intent())
            .unwrap_or_else(|error| panic!("surface: {error}"));
        assert_eq!(session.surface_nodes(), &[seq(0)]);
    }

    /// An assistant message advertising the tool call `call_id` (the
    /// lifecycle anchor the relationships gate demands behind tool/call and
    /// tool/result events).
    fn advertised_assistant(seq_value: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        SessionEvent {
            seq: seq(seq_value),
            time: i64::try_from(seq_value + 1).expect("seq fits i64"),
            data: SessionEventData::AssistantMessage(AssistantMessageData {
                turn,
                step,
                message: Message::Known(KnownMessage::Assistant {
                    id: MessageId::new(format!("a-{seq_value}")),
                    content: vec![ContentBlock::Known(KnownContentBlock::ToolCall {
                        id: ToolCallId::new(call_id),
                        name: "bash".to_owned(),
                        arguments: "{}".to_owned(),
                    })],
                    source: MessageSource::Known(KnownMessageSource::Model {
                        provider: "deepseek-official".to_owned(),
                        model: "deepseek-chat".to_owned(),
                        replay_state: None,
                        extra: RawObject::new(),
                    }),
                }),
                stream: Vec::new(),
                usage: None,
                interrupted: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    /// The plain `tool/call` companion (the demand side of the lifecycle).
    fn simple_tool_call(seq_value: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        SessionEvent {
            seq: seq(seq_value),
            time: i64::try_from(seq_value + 1).expect("seq fits i64"),
            data: SessionEventData::ToolCall(ToolCallData {
                turn,
                step,
                call_id: ToolCallId::new(call_id),
                name: "bash".to_owned(),
                arguments: "{}".to_owned(),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    /// The plain settling `tool/result` companion (the answer side).
    fn settling_tool_result(seq_value: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        SessionEvent {
            seq: seq(seq_value),
            time: i64::try_from(seq_value + 1).expect("seq fits i64"),
            data: SessionEventData::ToolResult(ToolResultData {
                turn,
                step,
                message: Message::Known(KnownMessage::Tool {
                    id: MessageId::new(format!("r-{seq_value}")),
                    content: vec![ContentBlock::Known(KnownContentBlock::Text {
                        text: "ok".to_owned(),
                    })],
                    source: MessageSource::Known(KnownMessageSource::Tool {
                        call_id: ToolCallId::new(call_id),
                        extra: RawObject::new(),
                    }),
                    tool_call_id: ToolCallId::new(call_id),
                    is_error: None,
                }),
                error: None,
                meta: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    /// One bare event with only its data and surface marker set (the seq
    /// and time slots are assigned by the chain builders).
    fn simple_event(data: SessionEventData, surface: bool) -> SessionEvent {
        SessionEvent {
            seq: seq(0),
            time: 1,
            data,
            ignorable: None,
            surface_op: surface.then_some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn pinned_event(data: SessionEventData, surface: bool) -> SessionEvent {
        SessionEvent {
            seq: seq(0),
            time: 1,
            data,
            ignorable: None,
            surface_op: surface.then_some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    /// Admission + dsh field anchor + both surface directions + file-level
    /// round trip for `turn/start` (dsh `SessionEventMap['turn/start']`:
    /// exactly `{ turn }`).
    #[test]
    fn turn_start_round_trips() {
        let data = SessionEventData::TurnStart(TurnStartData { turn: 1 });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "turn/start");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn"]);
        assert_log_only_role(&event.data);
        bracketed(event, Carrier::Bare);
    }

    /// `turn/end` (dsh: exactly `{ turn, reason }`).
    #[test]
    fn turn_end_round_trips() {
        let data = SessionEventData::TurnEnd(TurnEndData {
            turn: 1,
            reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
        });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "turn/end");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "reason"]);
        assert_log_only_role(&event.data);
        bracketed(event, Carrier::Turn);
    }

    /// `step/start` (dsh: exactly `{ turn, step }`).
    #[test]
    fn step_start_round_trips() {
        let data = SessionEventData::StepStart(StepBoundsData { turn: 1, step: 1 });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "step/start");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "step"]);
        assert_log_only_role(&event.data);
        bracketed(event, Carrier::StepOpen);
    }

    /// `step/end` (dsh: exactly `{ turn, step }`).
    #[test]
    fn step_end_round_trips() {
        let data = SessionEventData::StepEnd(StepBoundsData { turn: 1, step: 1 });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "step/end");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "step"]);
        assert_log_only_role(&event.data);
        bracketed(event, Carrier::Step);
    }

    /// `user/message` (dsh: the data IS the `UserMessage` object — no
    /// turn/step wrapper).
    #[test]
    fn user_message_round_trips() {
        let data = user_data("hello");
        let event = pinned_event(data, true);
        assert_eq!(event.event_type(), "user/message");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["role", "id", "content", "source"]);
        assert_surface_role(&event.data);
        bracketed(event, Carrier::Full);
    }

    /// `developer/message` (dsh: `{ turn, step, message, headerSeq? }`; the
    /// fixture carries no additions, so `headerSeq` stays absent).
    #[test]
    fn developer_message_round_trips() {
        let data = SessionEventData::DeveloperMessage(DeveloperMessageData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::Developer {
                id: MessageId::new("d-1"),
                content: vec![],
                source: MessageSource::Other(
                    serde_json::json!({"kind": "agent"})
                        .as_object()
                        .cloned()
                        .expect("object"),
                ),
            }),
            header_seq: None,
        });
        let event = pinned_event(data, true);
        assert_eq!(event.event_type(), "developer/message");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "step", "message"]);
        assert_surface_role(&event.data);
        bracketed(event, Carrier::Full);
    }

    /// `system/message` (dsh: exactly `{ turn, step, message }`).
    #[test]
    fn system_message_round_trips() {
        let data = SessionEventData::SystemMessage(SystemMessageData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::System {
                id: MessageId::new("s-1"),
                content: vec![ContentBlock::Known(KnownContentBlock::Text {
                    text: "prompt".to_owned(),
                })],
                source: MessageSource::Known(KnownMessageSource::SystemPrompt {
                    extra: RawObject::new(),
                }),
            }),
        });
        let event = pinned_event(data, true);
        assert_eq!(event.event_type(), "system/message");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "step", "message"]);
        assert_surface_role(&event.data);
        bracketed(event, Carrier::Full);
    }

    /// `assistant/message` (dsh: `{ turn, step, message, stream, usage?,
    /// interrupted? }`; the fixture carries neither optional field).
    #[test]
    fn assistant_message_round_trips() {
        let data = SessionEventData::AssistantMessage(AssistantMessageData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::Assistant {
                id: MessageId::new("a-1"),
                content: vec![ContentBlock::Known(KnownContentBlock::Text {
                    text: "reply".to_owned(),
                })],
                source: MessageSource::Known(KnownMessageSource::Model {
                    provider: "deepseek-official".to_owned(),
                    model: "deepseek-chat".to_owned(),
                    replay_state: None,
                    extra: RawObject::new(),
                }),
            }),
            stream: Vec::new(),
            usage: None,
            interrupted: None,
        });
        let event = pinned_event(data, true);
        assert_eq!(event.event_type(), "assistant/message");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "step", "message", "stream"]);
        assert_surface_role(&event.data);
        bracketed(event, Carrier::Full);
    }

    /// `assistant/attempt` (dsh: exactly `{ turn, step, stream }`).
    #[test]
    fn assistant_attempt_round_trips() {
        let data = SessionEventData::AssistantAttempt(AssistantAttemptData {
            turn: 1,
            step: 1,
            stream: Vec::new(),
        });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "assistant/attempt");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "step", "stream"]);
        assert_log_only_role(&event.data);
        bracketed(event, Carrier::Full);
    }

    /// `tool/call` (dsh: exactly `{ turn, step, callId, name, arguments }`).
    #[test]
    fn tool_call_round_trips() {
        let data = SessionEventData::ToolCall(ToolCallData {
            turn: 1,
            step: 1,
            call_id: ToolCallId::new("c-1"),
            name: "bash".to_owned(),
            arguments: "{}".to_owned(),
        });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "tool/call");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "step", "callId", "name", "arguments"]);
        assert_log_only_role(&event.data);
        bracketed(event, Carrier::ToolCall);
    }

    /// `tool/result` (dsh: `{ turn, step, message, error?, meta? }`; the
    /// fixture carries neither optional field).
    #[test]
    fn tool_result_round_trips() {
        let data = SessionEventData::ToolResult(ToolResultData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::Tool {
                id: MessageId::new("r-1"),
                content: vec![ContentBlock::Known(KnownContentBlock::Text {
                    text: "ok".to_owned(),
                })],
                source: MessageSource::Known(KnownMessageSource::Tool {
                    call_id: ToolCallId::new("c-1"),
                    extra: RawObject::new(),
                }),
                tool_call_id: ToolCallId::new("c-1"),
                is_error: None,
            }),
            error: None,
            meta: None,
        });
        let event = pinned_event(data, true);
        assert_eq!(event.event_type(), "tool/result");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["turn", "step", "message"]);
        assert_surface_role(&event.data);
        bracketed(event, Carrier::ToolResult);
    }

    /// `request/header` (dsh: `{ header, reason, startsSeries? }`; the
    /// fixture carries no series marker).
    #[test]
    fn request_header_round_trips() {
        let data = SessionEventData::RequestHeader(RequestHeaderData {
            header: EpochHeader {
                config: CallConfig {
                    provider: "deepseek-official".to_owned(),
                    model: "deepseek-chat".to_owned(),
                    controls: CallControls::default(),
                },
                adapter_defaults: None,
                tools: None,
            },
            reason: RequestHeaderReason::Initial,
            starts_series: None,
        });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "request/header");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["header", "reason"]);
        assert_log_only_role(&event.data);
        bracketed(event, Carrier::Full);
    }

    /// `request/context` (dsh: the data is the `RequestContext` snapshot —
    /// exactly `{ context }` wrapping `{ provider, model, contextWindow?,
    /// systemPromptUpdate? }`).
    #[test]
    fn request_context_round_trips() {
        let data = SessionEventData::RequestContext(RequestContextHolder {
            context: RequestContextData {
                provider: "deepseek-official".to_owned(),
                model: "deepseek-chat".to_owned(),
                context_window: None,
                system_prompt_update: None,
            },
        });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "request/context");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["context"]);
        let value = serde_json::to_value(&event).unwrap_or_default();
        let context = value
            .pointer("/data/context")
            .and_then(serde_json::Value::as_object)
            .expect("context object");
        let mut keys: Vec<&str> = context.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["model", "provider"]);
        assert_log_only_role(&event.data);
        bracketed(event, Carrier::Full);
    }

    /// `session/end-seed` (dsh: exactly `{ inherited?: true }`).
    #[test]
    fn session_end_seed_round_trips() {
        let data = SessionEventData::SessionEndSeed(EndSeedData {
            inherited: Some(TrueOnly),
        });
        let event = pinned_event(data, false);
        assert_eq!(event.event_type(), "session/end-seed");
        crate::admission::validate_event_data(&event)
            .unwrap_or_else(|error| panic!("admission: {error}"));
        assert_dsh_data_keys(&event, &["inherited"]);
        assert_log_only_role(&event.data);
        // The tagged marker demands a seeded log whose inherited count
        // covers it (marker at seq 0 -> exactly one inherited event).
        // The tagged marker demands a seeded log whose inherited count
        // covers it (marker at seq 0 -> exactly one inherited event).
        round_trips_on_disk("session-end-seed", true, 1, &[event], 0);
    }

    /// The five content blocks dsh admits in every role: the block axis
    /// shared by the role x block matrix (`tool-addition`/`tool-removal`
    /// are developer-only and ride the developer arm separately).
    fn universal_blocks() -> Vec<ContentBlock> {
        use fr_wire::content::{FileAttachmentRef, ImageAttachmentRef, ImageMediaType};
        vec![
            ContentBlock::Known(KnownContentBlock::Text {
                text: "t".to_owned(),
            }),
            ContentBlock::Known(KnownContentBlock::Reasoning {
                text: "r".to_owned(),
            }),
            ContentBlock::Known(KnownContentBlock::Image {
                attachment: ImageAttachmentRef {
                    attachment_id: fr_wire::ids::AttachmentId::new("att-1"),
                    media_type: ImageMediaType::Png,
                    bytes: 1,
                    width: 1,
                    height: 1,
                    name: None,
                    original_dimensions: None,
                },
                offloaded: None,
            }),
            ContentBlock::Known(KnownContentBlock::File {
                attachment: FileAttachmentRef {
                    attachment_id: fr_wire::ids::AttachmentId::new("att-2"),
                    name: "a.bin".to_owned(),
                    bytes: 1,
                },
            }),
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id: ToolCallId::new("call-x"),
                name: "bash".to_owned(),
                arguments: "{}".to_owned(),
            }),
        ]
    }

    /// The two developer-only tool-change blocks (dsh refuses them in every
    /// other role's content).
    fn tool_change_blocks() -> Vec<ContentBlock> {
        vec![
            ContentBlock::Known(KnownContentBlock::ToolAddition {
                tool_name: "bash".to_owned(),
            }),
            ContentBlock::Known(KnownContentBlock::ToolRemoval {
                tool_name: "bash".to_owned(),
            }),
        ]
    }

    /// The full legal chain for the role x block matrix: brackets, a
    /// request/header carrying the `bash` schema the developer additions
    /// reference, and the five role messages each carrying their blocks.
    fn role_block_chain() -> Vec<SessionEvent> {
        let mut events: Vec<SessionEvent> = Vec::new();
        let mut push = |seq_value: u64, data: SessionEventData, surface: bool| {
            events.push(simple_event_at(seq_value, data, surface));
        };
        push(
            0,
            SessionEventData::TurnStart(TurnStartData { turn: 1 }),
            false,
        );
        push(1, header_with_bash_schema(), false);
        push(
            2,
            SessionEventData::StepStart(StepBoundsData { turn: 1, step: 1 }),
            false,
        );
        push(3, system_message_data(), true);
        push(4, developer_message_data(), true);
        push(5, user_message_data_matrix(), true);
        push(6, assistant_message_data_matrix(), true);
        push(
            7,
            SessionEventData::ToolCall(ToolCallData {
                turn: 1,
                step: 1,
                call_id: ToolCallId::new("call-x"),
                name: "bash".to_owned(),
                arguments: "{}".to_owned(),
            }),
            false,
        );
        push(8, tool_result_message_data(), true);
        push(
            9,
            SessionEventData::StepEnd(StepBoundsData { turn: 1, step: 1 }),
            false,
        );
        push(
            10,
            SessionEventData::TurnEnd(TurnEndData {
                turn: 1,
                reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
            }),
            false,
        );
        events
    }

    fn simple_event_at(seq_value: u64, data: SessionEventData, surface: bool) -> SessionEvent {
        let mut event = simple_event(data, surface);
        event.seq = seq(seq_value);
        event.time = i64::try_from(seq_value + 1).expect("seq fits i64");
        event
    }

    fn header_with_bash_schema() -> SessionEventData {
        SessionEventData::RequestHeader(RequestHeaderData {
            header: EpochHeader {
                config: CallConfig {
                    provider: "deepseek-official".to_owned(),
                    model: "deepseek-chat".to_owned(),
                    controls: CallControls::default(),
                },
                adapter_defaults: None,
                tools: Some(vec![fr_wire::call_config::ToolSchema {
                    defer_loading: None,
                    name: "bash".to_owned(),
                    description: "d".to_owned(),
                    parameters: serde_json::json!({"type": "object"})
                        .as_object()
                        .cloned()
                        .expect("object"),
                }]),
            },
            reason: RequestHeaderReason::Initial,
            starts_series: None,
        })
    }

    fn system_message_data() -> SessionEventData {
        SessionEventData::SystemMessage(SystemMessageData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::System {
                id: MessageId::new("sys-1"),
                content: universal_blocks(),
                source: MessageSource::Known(KnownMessageSource::SystemPrompt {
                    extra: RawObject::new(),
                }),
            }),
        })
    }

    fn developer_message_data() -> SessionEventData {
        let mut content = universal_blocks();
        content.extend(tool_change_blocks());
        SessionEventData::DeveloperMessage(DeveloperMessageData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::Developer {
                id: MessageId::new("dev-1"),
                content,
                source: MessageSource::Other(
                    serde_json::json!({"kind": "agent"})
                        .as_object()
                        .cloned()
                        .expect("object"),
                ),
            }),
            header_seq: Some(seq(1)),
        })
    }

    fn user_message_data_matrix() -> SessionEventData {
        SessionEventData::UserMessage(Message::Known(KnownMessage::User {
            id: MessageId::new("usr-1"),
            content: universal_blocks(),
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
        }))
    }

    fn assistant_message_data_matrix() -> SessionEventData {
        SessionEventData::AssistantMessage(AssistantMessageData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::Assistant {
                id: MessageId::new("asst-1"),
                content: universal_blocks(),
                source: MessageSource::Known(KnownMessageSource::Model {
                    provider: "deepseek-official".to_owned(),
                    model: "deepseek-chat".to_owned(),
                    replay_state: None,
                    extra: RawObject::new(),
                }),
            }),
            stream: Vec::new(),
            usage: None,
            interrupted: None,
        })
    }

    fn tool_result_message_data() -> SessionEventData {
        SessionEventData::ToolResult(ToolResultData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::Tool {
                id: MessageId::new("tool-1"),
                content: universal_blocks(),
                source: MessageSource::Known(KnownMessageSource::Tool {
                    call_id: ToolCallId::new("call-x"),
                    extra: RawObject::new(),
                }),
                tool_call_id: ToolCallId::new("call-x"),
                is_error: None,
            }),
            error: None,
            meta: None,
        })
    }

    /// The role x block matrix at file level: all five roles carry the
    /// universal blocks (the developer arm additionally both tool-change
    /// blocks), and the stored log restores every role tag and block type
    /// verbatim while a misplace tool-change refuses.
    #[test]
    fn roles_and_blocks_round_trip() {
        const UNIVERSAL_TYPES: [&str; 5] = ["text", "reasoning", "image", "file", "tool-call"];
        const DEVELOPER_TYPES: [&str; 7] = [
            "text",
            "reasoning",
            "image",
            "file",
            "tool-call",
            "tool-addition",
            "tool-removal",
        ];
        let events = role_block_chain();
        let root = TempLog::new("roles-blocks");
        let id = fr_wire::ids::SessionId::new("roles-blocks");
        let mut meta = header();
        meta.id = id.clone();
        let compression = crate::persist::JsonlCompression::None;
        crate::persist::io::materialize(root.path(), &meta, 0, &events, compression)
            .unwrap_or_else(|error| panic!("materialize: {error}"));
        let path =
            crate::persist::filename::log_path(root.path(), None, &id, compression).expect("path");
        let outcome = crate::persist::read::read_stored_log(
            root.path(),
            &path,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .unwrap_or_else(|error| panic!("read: {error}"));
        assert_eq!(outcome.events, events);
        // Every role message restored its block types in order: 5 roles x
        // 5 universal blocks, and the developer arm additionally carries
        // both tool-change blocks (27 legal role x block instances).
        for event in &outcome.events {
            let Some(message) = role_message(event) else {
                continue;
            };
            let Message::Known(known) = message else {
                panic!("known role");
            };
            let types: Vec<String> = known
                .content()
                .iter()
                .map(|block| {
                    serde_json::to_value(block)
                        .unwrap_or_default()
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
            if known.role_name() == "developer" {
                assert_eq!(types, DEVELOPER_TYPES, "developer blocks");
            } else {
                assert_eq!(types, UNIVERSAL_TYPES, "role {}", known.role_name());
            }
        }
        // The placement rule is part of the matrix: a tool-change block
        // outside a developer message refuses at the stored-log gate.
        let mut smuggled = events;
        let SessionEventData::UserMessage(message) = &mut smuggled[5].data else {
            panic!("user row");
        };
        if let Message::Known(KnownMessage::User { content, .. }) = message {
            content.push(ContentBlock::Known(KnownContentBlock::ToolAddition {
                tool_name: "bash".to_owned(),
            }));
        }
        let smuggled_root = TempLog::new("roles-blocks-refusal");
        crate::persist::io::materialize(smuggled_root.path(), &meta, 0, &smuggled, compression)
            .unwrap_or_else(|error| panic!("materialize smuggled: {error}"));
        let smuggled_path =
            crate::persist::filename::log_path(smuggled_root.path(), None, &id, compression)
                .expect("smuggled path");
        let smuggled_error = crate::persist::read::read_stored_log(
            smuggled_root.path(),
            &smuggled_path,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .expect_err("tool-addition outside developer refuses");
        assert!(
            smuggled_error
                .message()
                .contains("tool-addition requires developer role")
        );
    }

    /// The message payload of a message-producing event, or `None` for the
    /// non-message rows of the chain.
    fn role_message(event: &SessionEvent) -> Option<&Message> {
        match &event.data {
            SessionEventData::UserMessage(message)
            | SessionEventData::SystemMessage(fr_wire::event::SystemMessageData {
                message, ..
            })
            | SessionEventData::DeveloperMessage(fr_wire::event::DeveloperMessageData {
                message,
                ..
            })
            | SessionEventData::AssistantMessage(fr_wire::event::AssistantMessageData {
                message,
                ..
            })
            | SessionEventData::ToolResult(fr_wire::event::ToolResultData { message, .. }) => {
                Some(message)
            }
            _ => None,
        }
    }

    #[test]
    fn ptc_subcall_records_stay_out_of_derived_messages() {
        // tool/ptc-dispatch* are plugin-owned log-only records (PRD 7.4):
        // deriveMessages excludes both the start marker and the settled
        // record.
        use fr_wire::event::UnknownEventData;
        let mut session = Session::new(header());
        for (time, type_name) in [(1, "tool/ptc-dispatch-start"), (2, "tool/ptc-dispatch")] {
            session
                .append_log_only(
                    time,
                    SessionEventData::Unknown(UnknownEventData {
                        type_name: type_name.to_owned(),
                        data: RawObject::new(),
                    }),
                )
                .unwrap_or_else(|error| panic!("log-only {type_name}: {error}"));
        }
        let messages = session.derive_messages();
        assert!(
            messages.is_empty(),
            "no ptc subcall message may project: {messages:?}"
        );
    }
}
