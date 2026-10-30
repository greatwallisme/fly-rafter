//! Shared wire shapes for the Fly Rafter plugin boundary (PRD 4.4/4.5).
//!
//! `fr-wire` is a host-independent pure-serde shape library: every plugin
//! artifact links its own copy, payloads cross the dylib boundary as JSON
//! bytes, and no pointer or state crosses with them. Wire field names and
//! values are protocol-wire-verbatim to dsh and therefore exempt from the
//! project naming rule (`snake_case` identifiers, verbatim camelCase keys).
//! Merge-extensible dsh unions carry an `Other` fallback that retains unknown
//! variants verbatim so a log written by a newer writer round-trips.

#![forbid(unsafe_code)]

pub mod base64;
pub mod call_config;
pub mod content;
pub mod event;
pub mod ids;
pub mod message;
pub mod services;
pub mod stream;

pub use call_config::{
    AdapterDefaults, CallConfig, CallControls, Failure, FinishReason, ReplayEnvelope, ToolHistory,
    ToolHistoryUpdate, ToolSchema,
};
pub use content::{
    ContentBlock, FileAttachmentRef, ImageAttachmentRef, ImageMediaType, KnownContentBlock,
    OriginalDimensions, RawObject, TrueOnly,
};
pub use event::{
    AppendOp, CancelCause, EndSeedData, EpochHeader, Origin, ReplaceOp, ReplaceTag,
    RequestContextData, RequestHeaderData, RequestHeaderReason, SessionEvent, SessionEventData,
    SessionHeader, StepBoundsData, SurfaceOp, ToolCallData, ToolResultData, ToolResultError,
    TurnEndReason, TurnEndReasonKind, TurnStartData, UnknownEventData, event_type,
};
pub use ids::{
    AttachmentId, LlmAttemptId, MessageId, ProviderRequestId, ReasoningEffortId, SAFE_INTEGER_MAX,
    SessionId, SessionLogOffset, SessionSeq, SessionSeqRangeError, ToolCallId,
};
pub use message::{
    ContextSnapshotSection, KnownMessage, KnownMessageSource, Message, MessageSource, ModelSource,
    Source, SystemPromptSource, ToolSource, UserSource,
};
pub use stream::{AssistantStreamRecord, KnownStreamChunk, StreamChunk};

// Rust guideline compliant 2026-10-23
