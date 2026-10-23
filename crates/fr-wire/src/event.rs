//! The session event envelope and its data variants (dsh `core/session`
//! types.ts verbatim wire shapes).
//!
//! The `type` key and the `data` object share one source: the envelope is
//! (de)serialized by hand so a known type and its data can never drift and
//! an unknown type is retained verbatim (`SessionEventData::Unknown`) —
//! matching the merge-extensible dsh log where plugin events and `plugin:`
//! records ride the same envelope.

use crate::call_config::{AdapterDefaults, CallConfig, Failure, SystemPromptUpdate, ToolSchema};
use crate::content::RawObject;
use crate::content::TrueOnly;
use crate::ids::{SessionId, SessionSeq, ToolCallId};
use crate::message::Message;
use crate::stream::AssistantStreamRecord;
use serde::{Deserialize, Serialize};

/// Why a turn ended: a known reason, or a plugin-added reason retained
/// verbatim (merge-extensible).
#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "the Other fallback embeds serde_json::Map (f64-containing Value), so Eq is not derivable"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TurnEndReason {
    /// A known reason.
    Known(TurnEndReasonKind),
    /// An unknown reason, retained verbatim.
    Other(RawObject),
}

/// Known turn-end reasons, tagged by the wire key `kind` (seven variants).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TurnEndReasonKind {
    /// The turn completed.
    #[serde(rename = "completed")]
    Completed,
    /// A cancellation request interrupted the live turn.
    #[serde(rename = "aborted")]
    Aborted {
        /// Why the active driver was cancelled.
        reason: CancelCause,
    },
    /// The turn was blocked.
    #[serde(rename = "blocked")]
    Blocked,
    /// The turn failed with a structured failure.
    #[serde(rename = "error")]
    Error {
        /// The failure facts.
        error: Failure,
    },
    /// At least one step reached its output-token ceiling.
    #[serde(rename = "max-tokens")]
    MaxTokens,
    /// A crash-orphaned turn was closed after the fact by resume; the loop
    /// never emits this reason live.
    #[serde(rename = "interrupted")]
    Interrupted,
    /// Fork-seed construction closed a turn still open at the fork boundary;
    /// only fork seeds carry this reason.
    #[serde(rename = "forked")]
    Forked,
}

/// Why an active agent driver was cancelled, tagged by the wire key `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CancelCause {
    /// Cancelled by the user.
    #[serde(rename = "user")]
    User,
    /// Cancelled by the parent agent.
    #[serde(rename = "parent")]
    Parent,
    /// Cancelled by a hook.
    #[serde(rename = "hook")]
    Hook {
        /// The hook's cancellation reason.
        reason: String,
    },
    /// Cancelled because the owning context was disposed.
    #[serde(rename = "disposed")]
    Disposed,
}

/// Why a `request/header` snapshot was appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RequestHeaderReason {
    /// The log's first header (a new conversation).
    #[serde(rename = "initial")]
    Initial,
    /// A loop instance's first request over a log that already has headers.
    #[serde(rename = "resume")]
    Resume,
    /// A later request used a different header.
    #[serde(rename = "change")]
    Change,
    /// An unchanged header began an explicitly distinct message series or
    /// followed a surface replacement.
    #[serde(rename = "series")]
    Series,
}

/// Canonical session-owner metadata header (logical `SessionHeader`); kept
/// outside the event log. The physical header line (delegationDepth always
/// present) is a persistence-layer shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    /// Logical format version (`SESSION_FORMAT_VERSION`, always 4 today).
    pub version: u64,
    /// The session's id, mirroring the session's single durable copy.
    pub id: SessionId,
    /// Non-negative safe-integer Unix epoch milliseconds at creation.
    pub created_at: i64,
    /// Absolute working directory the session was created in, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The session this one was forked from (seed lineage), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<SessionId>,
    /// Whether this session contains a fork-inherited event prefix.
    pub is_seeded: bool,
    /// Coarse product classification for a subagent child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Delegation depth: absent (zero) at top level, parent depth + 1 for a
    /// subagent child; persisted so a recursion budget survives restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_depth: Option<u64>,
    /// Id of the agent preset this session's agent was composed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
}

/// Coarse product classification of a session's creation origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// Created as a subagent child.
    #[serde(rename = "subagent")]
    Subagent,
}

/// Logged request state outside derived history: call config and tools.
///
/// The latest full `request/header` snapshot reconstructs the header;
/// canonical empty optional fields are absent, and a `system` key is
/// persistence-reserved (rejected at admission) because prompts are
/// `system/message` surface nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochHeader {
    /// The conversation's call configuration.
    pub config: CallConfig,
    /// Effective config fields materialized from the exact adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_defaults: Option<AdapterDefaults>,
    /// Assembled tool schemas; absent for a tool-less request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
}

/// Route metadata for the next request, logged only when the route,
/// capacity, or system prompt update mode changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContextData {
    /// Registered provider route the metadata belongs to.
    pub provider: String,
    /// Provider-owned model id the metadata belongs to.
    pub model: String,
    /// Maximum combined request and response context in tokens, when advertised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// The route's mid-conversation system prompt handling, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_update: Option<SystemPromptUpdate>,
}

/// Optional failure identity on a tool result; allowed only when the message
/// has `isError: true` (admission-enforced).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultError {
    /// Failure identity name (for example `ToolOutcomeUnknownError`).
    pub name: String,
    /// Stable machine-routing code (for example `TOOL_OUTCOME_UNKNOWN`).
    pub code: String,
    /// Raw user-facing reason, kept outside the model content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// How a session event entered the ordered surface: appended to the tail, or
/// a positional replacement over an inclusive surface-node range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SurfaceOp {
    /// Added to the surface tail (the normal append path).
    Append(AppendOp),
    /// Replaced surface nodes from `startSeq` through `endSeq` (inclusive).
    Replace(ReplaceOp),
}

/// The append marker; serializes as the bare string `"append"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppendOp;

impl Serialize for AppendOp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("append")
    }
}

impl<'de> Deserialize<'de> for AppendOp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == "append" {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("expected \"append\""))
        }
    }
}

/// The positional replacement operation; the wire object carries exactly the
/// keys `op`, `startSeq`, and `endSeq` (admission rejects extras).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceOp {
    /// The operation tag; always `"replace"`.
    pub op: ReplaceTag,
    /// Inclusive start surface-node seq; must reference an earlier event.
    pub start_seq: SessionSeq,
    /// Inclusive end surface-node seq; must reference an earlier event.
    pub end_seq: SessionSeq,
}

/// The replacement operation tag; serializes as the bare string `"replace"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplaceTag;

impl Serialize for ReplaceTag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("replace")
    }
}

impl<'de> Deserialize<'de> for ReplaceTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == "replace" {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("expected \"replace\""))
        }
    }
}

/// The `turn/start` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartData {
    /// The turn ordinal being opened.
    pub turn: u64,
}

/// The `turn/end` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEndData {
    /// The turn ordinal being closed.
    pub turn: u64,
    /// Why the turn ended.
    pub reason: TurnEndReason,
}

/// The `step/start` and `step/end` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepBoundsData {
    /// The owning turn ordinal.
    pub turn: u64,
    /// The step ordinal within the turn.
    pub step: u64,
}

/// The `developer/message` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeveloperMessageData {
    /// The owning turn ordinal.
    pub turn: u64,
    /// The owning step ordinal.
    pub step: u64,
    /// The developer message.
    pub message: Message,
    /// Earlier request/header defining every tool addition; required exactly
    /// when additions are present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_seq: Option<SessionSeq>,
}

/// The `system/message` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessageData {
    /// The owning turn ordinal.
    pub turn: u64,
    /// The owning step ordinal.
    pub step: u64,
    /// The rendered system-prompt message.
    pub message: Message,
}

/// The `assistant/message` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageData {
    /// The owning turn ordinal.
    pub turn: u64,
    /// The owning step ordinal.
    pub step: u64,
    /// The assembled assistant message.
    pub message: Message,
    /// Exact timed model stream, compacted without joining delta boundaries.
    pub stream: Vec<AssistantStreamRecord>,
    /// The step's token accounting, present when the adapter reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::call_config::TokenUsage>,
    /// Marks a cancelled-mid-stream settlement that carries the delivered
    /// prefix; undispatched tool calls are absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<TrueOnly>,
}

/// The `assistant/attempt` payload (no surface message was committed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantAttemptData {
    /// The owning turn ordinal.
    pub turn: u64,
    /// The owning step ordinal.
    pub step: u64,
    /// The attempt's compact stream.
    pub stream: Vec<AssistantStreamRecord>,
}

/// The `tool/call` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallData {
    /// The owning turn ordinal.
    pub turn: u64,
    /// The owning step ordinal.
    pub step: u64,
    /// Provider-issued call id pairing this call with its result.
    pub call_id: ToolCallId,
    /// The requested tool name.
    pub name: String,
    /// Raw unparsed arguments JSON string exactly as the model produced it.
    pub arguments: String,
}

/// The `tool/result` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultData {
    /// The owning turn ordinal.
    pub turn: u64,
    /// The owning step ordinal.
    pub step: u64,
    /// The tool-role result message.
    pub message: Message,
    /// Optional failure identity and reason, outside the model content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolResultError>,
    /// Tool-private presentation payload; JSON-serializable, absent unless
    /// the producing tool attaches one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

/// The `request/header` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestHeaderData {
    /// The full header snapshot for the next request.
    pub header: EpochHeader,
    /// Why this snapshot was appended.
    pub reason: RequestHeaderReason,
    /// Marks that this request begins a distinct model-message series,
    /// independently of the header reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_series: Option<TrueOnly>,
}

/// The `request/context` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContextHolder {
    /// The route metadata snapshot.
    pub context: RequestContextData,
}

/// The `session/end-seed` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndSeedData {
    /// Marks the fork-inherited cut; only the session constructor and fork
    /// seed construction may create a tagged marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited: Option<TrueOnly>,
}

/// One session event's `data` object: a known core payload, or an unknown
/// type's object retained verbatim (`plugin:` records and newer-writer
/// events ride this fallback and never enter derived history).
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEventData {
    /// `turn/start`.
    TurnStart(TurnStartData),
    /// `turn/end`.
    TurnEnd(TurnEndData),
    /// `step/start`.
    StepStart(StepBoundsData),
    /// `step/end`.
    StepEnd(StepBoundsData),
    /// `user/message`; the data IS the user message object (no turn/step
    /// wrapper), verbatim to dsh.
    UserMessage(Message),
    /// `developer/message`.
    DeveloperMessage(DeveloperMessageData),
    /// `system/message`.
    SystemMessage(SystemMessageData),
    /// `assistant/message`.
    AssistantMessage(AssistantMessageData),
    /// `assistant/attempt`.
    AssistantAttempt(AssistantAttemptData),
    /// `tool/call`.
    ToolCall(ToolCallData),
    /// `tool/result`.
    ToolResult(ToolResultData),
    /// `request/header`.
    RequestHeader(RequestHeaderData),
    /// `request/context`.
    RequestContext(RequestContextHolder),
    /// `session/end-seed`.
    SessionEndSeed(EndSeedData),
    /// An unknown event type, retained verbatim.
    Unknown(UnknownEventData),
}

/// An unknown event type's retained payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownEventData {
    /// The unrecognized type name.
    pub type_name: String,
    /// The data object exactly as stored.
    pub data: RawObject,
}

/// Wire type names of the known event data variants, in `type`-key form.
pub mod event_type {
    /// `turn/start`.
    pub const TURN_START: &str = "turn/start";
    /// `turn/end`.
    pub const TURN_END: &str = "turn/end";
    /// `step/start`.
    pub const STEP_START: &str = "step/start";
    /// `step/end`.
    pub const STEP_END: &str = "step/end";
    /// `user/message`.
    pub const USER_MESSAGE: &str = "user/message";
    /// `developer/message`.
    pub const DEVELOPER_MESSAGE: &str = "developer/message";
    /// `system/message`.
    pub const SYSTEM_MESSAGE: &str = "system/message";
    /// `assistant/message`.
    pub const ASSISTANT_MESSAGE: &str = "assistant/message";
    /// `assistant/attempt`.
    pub const ASSISTANT_ATTEMPT: &str = "assistant/attempt";
    /// `tool/call`.
    pub const TOOL_CALL: &str = "tool/call";
    /// `tool/result`.
    pub const TOOL_RESULT: &str = "tool/result";
    /// `request/header`.
    pub const REQUEST_HEADER: &str = "request/header";
    /// `request/context`.
    pub const REQUEST_CONTEXT: &str = "request/context";
    /// `session/end-seed`.
    pub const SESSION_END_SEED: &str = "session/end-seed";
}

/// The append-only source of truth for one agent interaction: an immutable
/// envelope over a `type` key and its single-sourced `data` object.
///
/// Serialized key order matches the dsh writer (`type`, `seq`, `time`,
/// `data`, then optional `sourceEventSeqs`, `surfaceOp`, `ignorable`) so a
/// byte-reproduction anchor stays stable under `preserve_order`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    /// Monotonic sequence number within the session.
    pub seq: SessionSeq,
    /// Unix epoch milliseconds.
    pub time: i64,
    /// The typed payload; its variant determines the wire `type` key.
    pub data: SessionEventData,
    /// Marks an event a reader may safely skip when it does not recognize
    /// the type; absent means required.
    pub ignorable: Option<TrueOnly>,
    /// How the event joined the ordered surface; present exactly on
    /// message-producing events.
    pub surface_op: Option<SurfaceOp>,
    /// Complete set of earlier source-event seqs this event cites.
    pub source_event_seqs: Option<Vec<SessionSeq>>,
}

impl SessionEvent {
    /// The wire `type` key for this event's data.
    #[must_use]
    pub fn event_type(&self) -> &str {
        self.data.type_name()
    }
}

impl SessionEventData {
    /// The wire `type` key for this variant; `Unknown` carries its own name.
    #[must_use]
    pub fn type_name(&self) -> &str {
        match self {
            Self::TurnStart(_) => event_type::TURN_START,
            Self::TurnEnd(_) => event_type::TURN_END,
            Self::StepStart(_) => event_type::STEP_START,
            Self::StepEnd(_) => event_type::STEP_END,
            Self::UserMessage(_) => event_type::USER_MESSAGE,
            Self::DeveloperMessage(_) => event_type::DEVELOPER_MESSAGE,
            Self::SystemMessage(_) => event_type::SYSTEM_MESSAGE,
            Self::AssistantMessage(_) => event_type::ASSISTANT_MESSAGE,
            Self::AssistantAttempt(_) => event_type::ASSISTANT_ATTEMPT,
            Self::ToolCall(_) => event_type::TOOL_CALL,
            Self::ToolResult(_) => event_type::TOOL_RESULT,
            Self::RequestHeader(_) => event_type::REQUEST_HEADER,
            Self::RequestContext(_) => event_type::REQUEST_CONTEXT,
            Self::SessionEndSeed(_) => event_type::SESSION_END_SEED,
            Self::Unknown(unknown) => &unknown.type_name,
        }
    }
}

impl Serialize for SessionEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let field_count = 4
            + usize::from(self.source_event_seqs.is_some())
            + usize::from(self.surface_op.is_some())
            + usize::from(self.ignorable.is_some());
        let mut map = serializer.serialize_map(Some(field_count))?;
        map.serialize_entry("type", self.event_type())?;
        map.serialize_entry("seq", &self.seq)?;
        map.serialize_entry("time", &self.time)?;
        map.serialize_entry("data", &self.data)?;
        if let Some(seqs) = &self.source_event_seqs {
            map.serialize_entry("sourceEventSeqs", seqs)?;
        }
        if let Some(op) = &self.surface_op {
            map.serialize_entry("surfaceOp", op)?;
        }
        if let Some(ignorable) = &self.ignorable {
            map.serialize_entry("ignorable", ignorable)?;
        }
        map.end()
    }
}

impl Serialize for SessionEventData {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::TurnStart(data) => data.serialize(serializer),
            Self::TurnEnd(data) => data.serialize(serializer),
            Self::StepStart(data) | Self::StepEnd(data) => data.serialize(serializer),
            Self::UserMessage(message) => message.serialize(serializer),
            Self::DeveloperMessage(data) => data.serialize(serializer),
            Self::SystemMessage(data) => data.serialize(serializer),
            Self::AssistantMessage(data) => data.serialize(serializer),
            Self::AssistantAttempt(data) => data.serialize(serializer),
            Self::ToolCall(data) => data.serialize(serializer),
            Self::ToolResult(data) => data.serialize(serializer),
            Self::RequestHeader(data) => data.serialize(serializer),
            Self::RequestContext(data) => data.serialize(serializer),
            Self::SessionEndSeed(data) => data.serialize(serializer),
            Self::Unknown(unknown) => unknown.data.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SessionEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawObject::deserialize(deserializer)?;
        Self::deserialize_from_map(raw).map_err(serde::de::Error::custom)
    }
}

impl SessionEvent {
    /// Rebuilds one event from its raw envelope object. The `type` key
    /// dispatches the `data` object to its typed payload; an unrecognized
    /// type keeps both name and data verbatim.
    fn deserialize_from_map(mut raw: RawObject) -> Result<Self, String> {
        let type_name = take_string(&mut raw, "type")?;
        let seq_value = take_value(&mut raw, "seq")?;
        let time_value = take_value(&mut raw, "time")?;
        let data_value = take_value(&mut raw, "data")?;
        let source_event_seqs = match raw.remove("sourceEventSeqs") {
            Some(value) => Some(
                serde_json::from_value::<Vec<SessionSeq>>(value).map_err(|err| err.to_string())?,
            ),
            None => None,
        };
        let surface_op = match raw.remove("surfaceOp") {
            Some(value) => {
                Some(serde_json::from_value::<SurfaceOp>(value).map_err(|err| err.to_string())?)
            }
            None => None,
        };
        let ignorable = match raw.remove("ignorable") {
            Some(value) => {
                Some(serde_json::from_value::<TrueOnly>(value).map_err(|err| err.to_string())?)
            }
            None => None,
        };
        if !raw.is_empty() {
            return Err(format!(
                "session event carries unknown envelope keys: {:#?}",
                raw.keys().collect::<Vec<_>>()
            ));
        }
        let seq: SessionSeq = serde_json::from_value(seq_value).map_err(|err| err.to_string())?;
        let time: i64 = serde_json::from_value(time_value).map_err(|err| err.to_string())?;
        let data = deserialize_data(&type_name, data_value)?;
        Ok(Self {
            seq,
            time,
            data,
            ignorable,
            surface_op,
            source_event_seqs,
        })
    }
}

/// Dispatches one `data` object to its typed payload by the event `type`.
fn deserialize_data(type_name: &str, data: serde_json::Value) -> Result<SessionEventData, String> {
    let from = |tag: &str| -> Result<(), String> {
        if type_name == tag {
            Ok(())
        } else {
            Err(format!("data does not match event type {type_name}"))
        }
    };
    Ok(match type_name {
        event_type::TURN_START => {
            from(event_type::TURN_START)?;
            SessionEventData::TurnStart(serde_json::from_value(data).map_err(|e| e.to_string())?)
        }
        event_type::TURN_END => {
            from(event_type::TURN_END)?;
            SessionEventData::TurnEnd(serde_json::from_value(data).map_err(|e| e.to_string())?)
        }
        event_type::STEP_START => {
            from(event_type::STEP_START)?;
            SessionEventData::StepStart(serde_json::from_value(data).map_err(|e| e.to_string())?)
        }
        event_type::STEP_END => {
            from(event_type::STEP_END)?;
            SessionEventData::StepEnd(serde_json::from_value(data).map_err(|e| e.to_string())?)
        }
        event_type::USER_MESSAGE => {
            SessionEventData::UserMessage(serde_json::from_value(data).map_err(|e| e.to_string())?)
        }
        event_type::DEVELOPER_MESSAGE => {
            from(event_type::DEVELOPER_MESSAGE)?;
            SessionEventData::DeveloperMessage(
                serde_json::from_value(data).map_err(|e| e.to_string())?,
            )
        }
        event_type::SYSTEM_MESSAGE => {
            from(event_type::SYSTEM_MESSAGE)?;
            SessionEventData::SystemMessage(
                serde_json::from_value(data).map_err(|e| e.to_string())?,
            )
        }
        event_type::ASSISTANT_MESSAGE => {
            from(event_type::ASSISTANT_MESSAGE)?;
            SessionEventData::AssistantMessage(
                serde_json::from_value(data).map_err(|e| e.to_string())?,
            )
        }
        event_type::ASSISTANT_ATTEMPT => {
            from(event_type::ASSISTANT_ATTEMPT)?;
            SessionEventData::AssistantAttempt(
                serde_json::from_value(data).map_err(|e| e.to_string())?,
            )
        }
        event_type::TOOL_CALL => {
            from(event_type::TOOL_CALL)?;
            SessionEventData::ToolCall(serde_json::from_value(data).map_err(|e| e.to_string())?)
        }
        event_type::TOOL_RESULT => {
            from(event_type::TOOL_RESULT)?;
            SessionEventData::ToolResult(serde_json::from_value(data).map_err(|e| e.to_string())?)
        }
        event_type::REQUEST_HEADER => {
            from(event_type::REQUEST_HEADER)?;
            SessionEventData::RequestHeader(
                serde_json::from_value(data).map_err(|e| e.to_string())?,
            )
        }
        event_type::REQUEST_CONTEXT => {
            from(event_type::REQUEST_CONTEXT)?;
            SessionEventData::RequestContext(
                serde_json::from_value(data).map_err(|e| e.to_string())?,
            )
        }
        event_type::SESSION_END_SEED => {
            from(event_type::SESSION_END_SEED)?;
            SessionEventData::SessionEndSeed(
                serde_json::from_value(data).map_err(|e| e.to_string())?,
            )
        }
        other => SessionEventData::Unknown(UnknownEventData {
            type_name: other.to_owned(),
            data: match data {
                serde_json::Value::Object(map) => map,
                _ => return Err(format!("event type {other} data must be an object")),
            },
        }),
    })
}

fn take_string(raw: &mut RawObject, key: &str) -> Result<String, String> {
    let value = raw
        .remove(key)
        .ok_or_else(|| format!("session event lacks {key}"))?;
    serde_json::from_value::<String>(value).map_err(|err| err.to_string())
}

fn take_value(raw: &mut RawObject, key: &str) -> Result<serde_json::Value, String> {
    raw.remove(key)
        .ok_or_else(|| format!("session event lacks {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{ContentBlock, KnownContentBlock};
    use crate::message::{KnownMessage, KnownMessageSource, MessageSource};

    fn seq(n: u64) -> SessionSeq {
        SessionSeq::new(n).unwrap_or_else(|e| panic!("seq {n} rejected: {e}"))
    }

    fn user_message() -> Message {
        Message::Known(KnownMessage::User {
            id: crate::ids::MessageId::new("m-1"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "hi".to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
        })
    }

    fn envelope(data: SessionEventData) -> SessionEvent {
        SessionEvent {
            seq: seq(3),
            time: 1_700_000_000_000,
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    #[test]
    fn turn_start_wires_exact_key_order_and_shape() {
        let json = serde_json::to_string(&envelope(SessionEventData::TurnStart(TurnStartData {
            turn: 1,
        })))
        .unwrap_or_default();
        assert_eq!(
            json,
            r#"{"type":"turn/start","seq":3,"time":1700000000000,"data":{"turn":1}}"#
        );
    }

    #[test]
    fn developer_message_data_wraps_turn_step_and_optional_header_seq() {
        // E2 (PRD 7.1): developer/message data carries the {turn, step, message,
        // headerSeq?} wrapper the user/message payload deliberately lacks.
        let wrapped = serde_json::json!({
            "type": "developer/message",
            "seq": 3,
            "time": 10,
            "data": {
                "turn": 1,
                "step": 2,
                "headerSeq": 1,
                "message": {
                    "id": "d1",
                    "role": "developer",
                    "content": [],
                    "source": {"kind": "agent"}
                }
            }
        });
        let event: SessionEvent = serde_json::from_value(wrapped.clone()).expect("developer event");
        let SessionEventData::DeveloperMessage(data) = &event.data else {
            panic!("developer payload");
        };
        assert_eq!((data.turn, data.step), (1, 2));
        assert_eq!(data.header_seq.map(crate::SessionSeq::get), Some(1));
        let text = serde_json::to_string(&event).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("reparse");
        assert_eq!(parsed, wrapped);
    }

    #[test]
    fn user_message_data_is_the_message_itself() {
        let event = envelope(SessionEventData::UserMessage(user_message()));
        let json = serde_json::to_value(&event).unwrap_or_else(|_| serde_json::Value::Null);
        let data = json.get("data").unwrap_or_else(|| panic!("data present"));
        assert_eq!(data["role"], serde_json::json!("user"));
        // The erratum's contrast arm: the user payload has no turn wrapper,
        // while every other surface event keeps one.
        assert!(data.get("step").is_none());
        assert_eq!(json["type"], serde_json::json!("user/message"));
        let back: SessionEvent =
            serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, event);

        // The developer payload keeps the {turn, step, message} wrapper.
        let developer = envelope(SessionEventData::DeveloperMessage(DeveloperMessageData {
            turn: 1,
            step: 2,
            message: user_message(),
            header_seq: None,
        }));
        let json = serde_json::to_value(&developer).unwrap_or_else(|_| serde_json::Value::Null);
        let data = json.get("data").unwrap_or_else(|| panic!("data present"));
        assert_eq!(data["turn"], serde_json::json!(1));
        assert_eq!(data["step"], serde_json::json!(2));
        assert_eq!(data["message"]["role"], serde_json::json!("user"));
    }

    #[test]
    fn assistant_message_carries_usage_and_interrupted_markers() {
        let event = envelope(SessionEventData::AssistantMessage(AssistantMessageData {
            turn: 1,
            step: 0,
            message: user_message(),
            stream: vec![],
            usage: Some(crate::call_config::TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                ..crate::call_config::TokenUsage::default()
            }),
            interrupted: Some(TrueOnly),
        }));
        let json = serde_json::to_value(&event).unwrap_or_else(|_| serde_json::Value::Null);
        let data = json.get("data").unwrap_or_else(|| panic!("data present"));
        assert_eq!(data["usage"]["inputTokens"], serde_json::json!(10));
        assert_eq!(data["interrupted"], serde_json::Value::Bool(true));
        let back: SessionEvent =
            serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, event);
    }

    #[test]
    fn tool_result_wires_error_identity_outside_content() {
        let event = envelope(SessionEventData::ToolResult(ToolResultData {
            turn: 1,
            step: 0,
            message: user_message(),
            error: Some(ToolResultError {
                name: "ToolNotStartedError".to_owned(),
                code: "TOOL_NOT_STARTED".to_owned(),
                reason: None,
            }),
            meta: None,
        }));
        let json = serde_json::to_value(&event).unwrap_or_else(|_| serde_json::Value::Null);
        let error = json["data"]["error"].clone();
        assert_eq!(error["code"], serde_json::json!("TOOL_NOT_STARTED"));
        assert!(error.get("reason").is_none());
        let back: SessionEvent =
            serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, event);
    }

    #[test]
    fn request_header_round_trips_canonical_snapshot() {
        let event = envelope(SessionEventData::RequestHeader(RequestHeaderData {
            header: EpochHeader {
                config: CallConfig {
                    provider: "deepseek-official".to_owned(),
                    model: "deepseek-flash".to_owned(),
                    controls: crate::call_config::CallControls::default(),
                },
                adapter_defaults: None,
                tools: None,
            },
            reason: RequestHeaderReason::Initial,
            starts_series: None,
        }));
        let json = serde_json::to_value(&event).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(json["data"]["reason"], serde_json::json!("initial"));
        assert!(json["data"]["header"].get("tools").is_none());
        assert!(json["data"]["header"].get("system").is_none());
        let back: SessionEvent =
            serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, event);
    }

    #[test]
    fn end_seed_round_trips_both_marked_and_plain() {
        let plain = envelope(SessionEventData::SessionEndSeed(EndSeedData {
            inherited: None,
        }));
        let json = serde_json::to_value(&plain).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(json["data"], serde_json::json!({}));
        let marked = envelope(SessionEventData::SessionEndSeed(EndSeedData {
            inherited: Some(TrueOnly),
        }));
        let json = serde_json::to_value(&marked).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(json["data"], serde_json::json!({"inherited": true}));
    }

    #[test]
    fn surface_op_replace_wires_camel_case_bounds() {
        let event = envelope(SessionEventData::ToolResult(ToolResultData {
            turn: 1,
            step: 0,
            message: user_message(),
            error: None,
            meta: None,
        }));
        let mut event = event;
        event.surface_op = Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: seq(0),
            end_seq: seq(2),
        }));
        event.source_event_seqs = Some(vec![seq(0), seq(1), seq(2)]);
        let json = serde_json::to_value(&event).unwrap_or_else(|_| serde_json::Value::Null);
        assert_eq!(json["surfaceOp"]["startSeq"], serde_json::json!(0));
        assert_eq!(json["sourceEventSeqs"], serde_json::json!([0, 1, 2]));
        let back: SessionEvent =
            serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip"));
        assert_eq!(back, event);
    }

    #[test]
    fn unknown_event_type_is_retained_verbatim_with_ignorable() {
        let raw = serde_json::json!({
            "type": "plugin:example/state",
            "seq": 9,
            "time": 42,
            "data": {"k": "v", "n": 1},
            "ignorable": true
        });
        let event: SessionEvent =
            serde_json::from_value(raw.clone()).unwrap_or_else(|_| panic!("retained"));
        match &event.data {
            SessionEventData::Unknown(unknown) => {
                assert_eq!(unknown.type_name, "plugin:example/state");
                assert_eq!(unknown.data.get("k"), Some(&serde_json::json!("v")));
            }
            _ => panic!("unknown must not parse as known"),
        }
        assert_eq!(event.event_type(), "plugin:example/state");
        assert_eq!(
            serde_json::to_value(&event).unwrap_or_else(|_| serde_json::Value::Null),
            raw
        );
    }

    #[test]
    fn turn_end_unknown_reason_is_retained() {
        let event = envelope(SessionEventData::TurnEnd(TurnEndData {
            turn: 2,
            reason: TurnEndReason::Other(
                serde_json::json!({"kind": "policy"})
                    .as_object()
                    .map_or_else(|| panic!("object"), std::clone::Clone::clone),
            ),
        }));
        let back: SessionEvent = serde_json::to_value(&event).map_or_else(
            |_| unreachable!("serialize"),
            |json| serde_json::from_value(json).unwrap_or_else(|_| panic!("round trip")),
        );
        assert_eq!(back, event);
    }

    #[test]
    fn unknown_envelope_key_is_rejected() {
        let raw = serde_json::json!({
            "type": "turn/start",
            "seq": 0,
            "time": 1,
            "data": {"turn": 1},
            "seedLength": 5
        });
        let err = serde_json::from_value::<SessionEvent>(raw).expect_err("unknown envelope key");
        assert!(err.to_string().contains("unknown envelope keys"));
    }
}
