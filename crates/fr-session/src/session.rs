//! The in-memory session: append-only log, incremental surface fold, the
//! derived-message cache, and the request-header fold (dsh core/session
//! index.ts Session semantics on the typed fr-wire event model).
//!
//! Append-time reentrancy is impossible by construction: `&mut self` serializes
//! every mutation. The event timestamp is an explicit parameter (the clock
//! seam; the host supplies wall time).

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
        let mut session = Self::new(header);
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
}
