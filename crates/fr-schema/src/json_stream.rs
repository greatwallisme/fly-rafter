// Rust guideline compliant 2026-11-04
//! The `--json` event-stream contract (headless spec, 2026-11-04 18b Q5A
//! ruling): eight frame schema documents plus a validator over the
//! newline-delimited `fr headless --json` stdout face.
//!
//! dsh ships no schema artifact for its `-- command` run-events face, so the
//! documents are an fr addition defined against the dsh projection code
//! (`json-stream.ts`): a projected event is a JSON object with a string
//! `type`; `status` frames vary their optional fields by `phase`, and every
//! frame except `final` may carry the bounding pass's `truncated: true`
//! flag. One module-local declaration tree derives both faces — the schema
//! document constants and the validator — so they cannot drift, mirroring
//! the crate's single-source discipline.

use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::schema::Violation;

/// The eight projected frame kinds, keyed by the wire `type` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameKind {
    /// The opening frame: session identity and working directory.
    Session,
    /// Turn and step phase progress (and the step's token accounting).
    Status,
    /// Committed visible text from one assistant message.
    Text,
    /// Committed reasoning from one assistant message.
    Thinking,
    /// One model-issued tool invocation, arguments re-parsed.
    ToolCall,
    /// One tool settlement, appended results only.
    ToolResult,
    /// A run-level failure; the stream carries no `final` after it.
    Error,
    /// The lossless terminal answer.
    Final,
}

impl FrameKind {
    /// Every frame kind, in the projection's introduction order.
    pub const ALL: [Self; 8] = [
        Self::Session,
        Self::Status,
        Self::Text,
        Self::Thinking,
        Self::ToolCall,
        Self::ToolResult,
        Self::Error,
        Self::Final,
    ];

    /// The wire `type` key.
    #[must_use]
    pub const fn type_key(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Status => "status",
            Self::Text => "text",
            Self::Thinking => "thinking",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Error => "error",
            Self::Final => "final",
        }
    }

    /// The frame kind for one wire `type` key; `None` when unknown.
    ///
    /// Not a `const fn`: matching on string slices inside constant
    /// evaluation is not stable on the pinned toolchain.
    #[must_use]
    pub fn from_type_key(key: &str) -> Option<Self> {
        match key {
            "session" => Some(Self::Session),
            "status" => Some(Self::Status),
            "text" => Some(Self::Text),
            "thinking" => Some(Self::Thinking),
            "tool_call" => Some(Self::ToolCall),
            "tool_result" => Some(Self::ToolResult),
            "error" => Some(Self::Error),
            "final" => Some(Self::Final),
            _ => None,
        }
    }
}

impl std::fmt::Display for FrameKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_key())
    }
}

/// One declared object key in a frame declaration.
struct Prop {
    name: &'static str,
    required: bool,
    decl: Decl,
}

/// A module-local frame declaration: the single source both the schema
/// document and the validator derive from (the crate's declaration
/// discipline, scoped to the shapes the projection emits).
enum Decl {
    /// Any JSON value (the re-parsed tool-call arguments).
    Any,
    /// JSON boolean (the bounding pass's flag).
    Boolean,
    /// JSON integer with an inclusive lower bound.
    Integer {
        /// Inclusive lower bound.
        min: Option<i64>,
    },
    /// UTF-8 string; a non-empty candidate list restricts membership.
    String {
        /// Allowed values; empty means any string.
        candidates: Vec<&'static str>,
    },
    /// Object with declared keys; unknown keys are rejected unless
    /// `additional_properties` allows them (merge-extensible payloads).
    Object {
        /// Declared keys in projection order.
        properties: Vec<Prop>,
        /// Whether undeclared keys are allowed.
        additional_properties: bool,
    },
}

impl Decl {
    /// The JSON Schema 2020-12 document for this declaration.
    fn document(&self) -> Value {
        match self {
            Self::Any => Value::Bool(true),
            Self::Boolean => serde_json::json!({ "type": "boolean" }),
            Self::Integer { min } => min.map_or_else(
                || serde_json::json!({ "type": "integer" }),
                |min| serde_json::json!({ "type": "integer", "minimum": min }),
            ),
            Self::String { candidates } => match candidates.as_slice() {
                [] => serde_json::json!({ "type": "string" }),
                names => serde_json::json!({ "type": "string", "enum": names }),
            },
            Self::Object {
                properties,
                additional_properties,
            } => {
                let mut defs = Map::new();
                let mut required = Vec::new();
                for prop in properties {
                    defs.insert(prop.name.to_owned(), prop.decl.document());
                    if prop.required {
                        required.push(Value::from(prop.name));
                    }
                }
                serde_json::json!({
                    "type": "object",
                    "properties": defs,
                    "required": required,
                    "additionalProperties": additional_properties,
                })
            }
        }
    }

    /// Validates one value against this declaration, appending violations.
    fn check(&self, value: &Value, path: &str, out: &mut Vec<Violation>) {
        match self {
            Self::Any => {}
            Self::Boolean => {
                if !value.is_boolean() {
                    push(out, path, "expected boolean");
                }
            }
            Self::Integer { min } => match value.as_i64() {
                Some(int) => {
                    if min.is_some_and(|bound| int < bound) {
                        push(out, path, "integer below the declared minimum");
                    }
                }
                None => push(out, path, "expected integer"),
            },
            Self::String { candidates } => match value.as_str() {
                Some(text) => {
                    if !candidates.is_empty() && !candidates.contains(&text) {
                        push(out, path, "value not in candidates");
                    }
                }
                None => push(out, path, "expected string"),
            },
            Self::Object {
                properties,
                additional_properties,
            } => {
                let Some(object) = value.as_object() else {
                    push(out, path, "expected object");
                    return;
                };
                for prop in properties {
                    match object.get(prop.name) {
                        Some(item) => {
                            let child = child_path(path, prop.name);
                            prop.decl.check(item, &child, out);
                        }
                        None if prop.required => {
                            push(out, path, &format!("missing required key `{}`", prop.name));
                        }
                        None => {}
                    }
                }
                if !additional_properties {
                    for key in object.keys() {
                        if !properties.iter().any(|prop| prop.name == key) {
                            push(out, path, &format!("unknown key `{key}`"));
                        }
                    }
                }
            }
        }
    }
}

/// The empty path for the root value (the [`Violation`] path convention).
const fn root_path() -> &'static str {
    ""
}

/// Joins one object key onto a violation path.
fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// Records one violation at `path`.
fn push(out: &mut Vec<Violation>, path: &str, message: &str) {
    out.push(Violation {
        path: path.to_owned(),
        message: message.to_owned(),
    });
}

/// The usage sub-object: provider token accounting, optional buckets only
/// when the adapter reported them (the dsh `StepUsage` shape).
fn usage_decl() -> Decl {
    Decl::Object {
        properties: vec![
            Prop {
                name: "inputTokens",
                required: true,
                decl: Decl::Integer { min: Some(0) },
            },
            Prop {
                name: "outputTokens",
                required: true,
                decl: Decl::Integer { min: Some(0) },
            },
            Prop {
                name: "totalTokens",
                required: false,
                decl: Decl::Integer { min: Some(0) },
            },
            Prop {
                name: "cacheReadTokens",
                required: false,
                decl: Decl::Integer { min: Some(0) },
            },
            Prop {
                name: "cacheWriteTokens",
                required: false,
                decl: Decl::Integer { min: Some(0) },
            },
            Prop {
                name: "reasoningTokens",
                required: false,
                decl: Decl::Integer { min: Some(0) },
            },
        ],
        additional_properties: false,
    }
}

/// The `turn_end` reason sub-object: a known `kind` or a retained-verbatim
/// unknown, so `kind` is any string and extras ride the open face.
fn reason_decl() -> Decl {
    Decl::Object {
        properties: vec![Prop {
            name: "kind",
            required: true,
            decl: Decl::String { candidates: vec![] },
        }],
        additional_properties: true,
    }
}

/// The `truncated` flag property the bounding pass may add to any frame
/// except `final` (which is exempt from bounding).
const fn truncated_prop() -> Prop {
    Prop {
        name: "truncated",
        required: false,
        decl: Decl::Boolean,
    }
}

/// One frame's declaration, keyed by kind (the validator and the document
/// both read this one tree). Per-kind builders keep every function small.
fn frame_decl(kind: FrameKind) -> Decl {
    match kind {
        FrameKind::Session => {
            bounded_object(vec![type_prop("session"), session_id_prop(), cwd_prop()])
        }
        FrameKind::Status => status_decl(),
        FrameKind::Text => bounded_object(vec![type_prop("text"), text_prop()]),
        FrameKind::Thinking => bounded_object(vec![type_prop("thinking"), text_prop()]),
        FrameKind::ToolCall => bounded_object(vec![
            type_prop("tool_call"),
            call_id_prop(),
            required_string("tool"),
            Prop {
                name: "input",
                required: true,
                decl: Decl::Any,
            },
        ]),
        FrameKind::ToolResult => bounded_object(vec![
            type_prop("tool_result"),
            call_id_prop(),
            Prop {
                name: "status",
                required: true,
                decl: Decl::String {
                    candidates: vec!["completed", "error"],
                },
            },
            required_string("result"),
        ]),
        FrameKind::Error => bounded_object(vec![type_prop("error"), required_string("message")]),
        // The terminal answer is the lossless contract: never bounded, so
        // no `truncated` flag exists on this frame.
        FrameKind::Final => Decl::Object {
            properties: vec![type_prop("final"), text_prop()],
            additional_properties: false,
        },
    }
}

/// The wire `type` key property pinned to one frame name (not a `const
/// fn`: `vec!` allocation is not const-stable).
fn type_prop(name: &'static str) -> Prop {
    Prop {
        name: "type",
        required: true,
        decl: Decl::String {
            candidates: vec![name],
        },
    }
}

/// A required string property.
const fn required_string(name: &'static str) -> Prop {
    Prop {
        name,
        required: true,
        decl: Decl::String { candidates: vec![] },
    }
}

/// A required string property named `text` (the visible/reasoning payload).
const fn text_prop() -> Prop {
    required_string("text")
}

/// A required string property named `callId`.
const fn call_id_prop() -> Prop {
    required_string("callId")
}

/// A required string property named `sessionId`.
const fn session_id_prop() -> Prop {
    required_string("sessionId")
}

/// A required string property named `cwd`.
const fn cwd_prop() -> Prop {
    required_string("cwd")
}

/// A bounded frame object: declared keys, `truncated` allowed, nothing else.
fn bounded_object(properties: Vec<Prop>) -> Decl {
    let mut properties = properties;
    properties.push(truncated_prop());
    Decl::Object {
        properties,
        additional_properties: false,
    }
}

/// The `status` frame: phase-conditional optional fields with the honest
/// minimal presence rules (turn always present; step/usage/reason only on
/// their phases).
fn status_decl() -> Decl {
    Decl::Object {
        properties: vec![
            type_prop("status"),
            Prop {
                name: "phase",
                required: true,
                decl: Decl::String {
                    candidates: vec!["turn_start", "step_start", "step_end", "turn_end"],
                },
            },
            Prop {
                name: "turn",
                required: true,
                decl: Decl::Integer { min: Some(0) },
            },
            Prop {
                name: "step",
                required: false,
                decl: Decl::Integer { min: Some(0) },
            },
            Prop {
                name: "usage",
                required: false,
                decl: usage_decl(),
            },
            Prop {
                name: "reason",
                required: false,
                decl: reason_decl(),
            },
            truncated_prop(),
        ],
        additional_properties: false,
    }
}

/// The eight schema documents, keyed by frame kind (lazy constants; the
/// declaration tree is the single source).
static FRAME_DOCUMENTS: LazyLock<[Value; 8]> =
    LazyLock::new(|| std::array::from_fn(|index| frame_decl(FrameKind::ALL[index]).document()));

/// The JSON Schema 2020-12 document constant for one frame kind.
#[must_use]
pub fn frame_document(kind: FrameKind) -> &'static Value {
    let position = FrameKind::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or_else(|| unreachable!("every FrameKind variant is listed in `FrameKind::ALL`"));
    &FRAME_DOCUMENTS[position]
}

/// Validates one projected frame value against its declaration, returning
/// every violation (empty when the value conforms).
///
/// # Errors
///
/// Never; the violation list is the failure face (an empty list means the
/// value conforms). The signature stays total so callers can collect all
/// violations in one pass.
#[must_use]
pub fn validate_frame(kind: FrameKind, value: &Value) -> Vec<Violation> {
    let mut violations = Vec::new();
    frame_decl(kind).check(value, root_path(), &mut violations);
    violations
}

/// Validates one newline-delimited stream line: dispatches on the wire
/// `type` key and checks the matching frame declaration.
///
/// # Errors
///
/// Never; the violation list is the failure face (unknown, missing, or
/// non-string `type` values are violations, not errors).
pub fn validate_line(value: &Value) -> Result<FrameKind, Vec<Violation>> {
    let mut violations = Vec::new();
    let key = value.as_object().and_then(|object| object.get("type"));
    let Some(key) = key.and_then(Value::as_str) else {
        push(&mut violations, "type", "expected a string `type` key");
        return Err(violations);
    };
    let Some(kind) = FrameKind::from_type_key(key) else {
        push(&mut violations, "type", "unknown frame type");
        return Err(violations);
    };
    frame_decl(kind).check(value, root_path(), &mut violations);
    if violations.is_empty() {
        Ok(kind)
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameKind, frame_document, validate_frame, validate_line};
    use serde_json::{Value, json};

    /// Builds one canonical sample per frame kind, in `ALL` order.
    fn samples() -> Vec<(FrameKind, Value)> {
        vec![
            (
                FrameKind::Session,
                json!({"type": "session", "sessionId": "session-u", "cwd": "/repo"}),
            ),
            (
                FrameKind::Status,
                json!({"type": "status", "phase": "step_end", "turn": 1, "step": 2}),
            ),
            (FrameKind::Text, json!({"type": "text", "text": "answer"})),
            (
                FrameKind::Thinking,
                json!({"type": "thinking", "text": "reason"}),
            ),
            (
                FrameKind::ToolCall,
                json!({"type": "tool_call", "callId": "c1", "tool": "bash", "input": {"cmd": "ls"}}),
            ),
            (
                FrameKind::ToolResult,
                json!({"type": "tool_result", "callId": "c1", "status": "completed", "result": "ok"}),
            ),
            (
                FrameKind::Error,
                json!({"type": "error", "message": "boom"}),
            ),
            (
                FrameKind::Final,
                json!({"type": "final", "text": "the answer"}),
            ),
        ]
    }

    #[test]
    fn every_frame_document_validates_its_canonical_sample() {
        for (kind, sample) in samples() {
            assert!(
                validate_frame(kind, &sample).is_empty(),
                "{kind} document rejects its own sample: {:?}",
                validate_frame(kind, &sample)
            );
            assert_eq!(validate_line(&sample), Ok(kind), "{kind} line dispatch");
        }
    }

    #[test]
    fn documents_carry_the_schema_shape() {
        let session = frame_document(FrameKind::Session);
        assert_eq!(session["type"], "object");
        assert_eq!(session["required"], json!(["type", "sessionId", "cwd"]));
        assert_eq!(session["additionalProperties"], false);
        assert_eq!(session["properties"]["type"]["enum"], json!(["session"]));
        // The re-parsed arguments accept any JSON value.
        assert_eq!(
            frame_document(FrameKind::ToolCall)["properties"]["input"],
            json!(true)
        );
        // Usage buckets are non-negative integers, optional unless reported.
        let usage = frame_document(FrameKind::Status)["properties"]["usage"].clone();
        assert_eq!(usage["properties"]["inputTokens"]["type"], "integer");
        assert_eq!(usage["required"], json!(["inputTokens", "outputTokens"]));
    }

    #[test]
    fn violations_name_the_offending_path() {
        let bad = json!({"type": "text", "text": 7});
        let violations = validate_line(&bad).expect_err("type mismatch");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, "text");
    }

    #[test]
    fn undeclared_keys_are_rejected() {
        let bad = json!({"type": "final", "text": "a", "truncated": true});
        let violations = validate_line(&bad).expect_err("final is exempt from bounding");
        assert_eq!(violations[0].path, "");
        assert_eq!(violations[0].message, "unknown key `truncated`");
    }

    #[test]
    fn phase_candidates_are_pinned() {
        let bad = json!({"type": "status", "phase": "pause", "turn": 1});
        let violations = validate_line(&bad).expect_err("unknown phase");
        assert_eq!(violations[0].path, "phase");
    }

    #[test]
    fn unknown_reason_kinds_ride_the_open_face() {
        let known = json!({"type": "status", "phase": "turn_end", "turn": 1, "reason": {"kind": "completed"}});
        assert!(matches!(validate_line(&known), Ok(FrameKind::Status)));
        let unknown = json!({"type": "status", "phase": "turn_end", "turn": 1, "reason": {"kind": "crash", "detail": 1}});
        assert!(matches!(validate_line(&unknown), Ok(FrameKind::Status)));
        let missing = json!({"type": "status", "phase": "turn_end", "turn": 1, "reason": {}});
        assert_eq!(
            validate_line(&missing).expect_err("kind missing")[0].path,
            "reason"
        );
    }

    #[test]
    fn line_dispatch_rejects_missing_and_unknown_types() {
        let missing = json!({"sessionId": "s"});
        assert_eq!(
            validate_line(&missing).expect_err("no type key")[0].path,
            "type"
        );
        let unknown = json!({"type": "summary"});
        assert_eq!(
            validate_line(&unknown).expect_err("unknown type")[0].message,
            "unknown frame type"
        );
        let not_object = json!("text");
        assert_eq!(
            validate_line(&not_object).expect_err("not an object")[0].path,
            "type"
        );
    }

    #[test]
    fn status_step_and_usage_fields_are_optional() {
        let turn_only = json!({"type": "status", "phase": "turn_start", "turn": 3});
        assert!(matches!(validate_line(&turn_only), Ok(FrameKind::Status)));
        let usage = json!({"type": "status", "phase": "step_end", "turn": 3, "step": 1, "usage": {"inputTokens": 1, "outputTokens": 2, "reasoningTokens": 0}});
        assert!(matches!(validate_line(&usage), Ok(FrameKind::Status)));
    }
}
