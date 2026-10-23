//! The physical header line: serialization, shape guard, and the refusals
//! that precede any event decoding (dsh format.ts `HeaderLine` family).

use serde_json::{Value, json};

use super::{PersistError, session_format_version_refusal};
use fr_wire::event::{Origin, SessionHeader};
use fr_wire::ids::{SAFE_INTEGER_MAX, SessionId};

/// The current physical header's required keys.
const HEADER_REQUIRED_KEYS: [&str; 6] = [
    "type",
    "version",
    "id",
    "createdAt",
    "isSeeded",
    "delegationDepth",
];

/// The physical header's optional keys.
const HEADER_OPTIONAL_KEYS: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];

/// Builds the header line object from a logical [`SessionHeader`].
///
/// The exact inherited cut lives on the last tagged `session/end-seed`
/// event, not on the header line; `inherited_event_count` is validated
/// here with the same refusal the physical writer raises (dsh
/// `toHeaderLine`).
///
/// # Errors
///
/// Returns `Corruption` when a seeded header lacks its cut or an unseeded
/// header carries a nonzero cut.
pub fn to_header_line(
    header: &SessionHeader,
    inherited_event_count: Option<u64>,
) -> Result<Value, PersistError> {
    if header.is_seeded && inherited_event_count.is_none() {
        return Err(PersistError::Corruption(
            "seeded session header requires an inherited event count".to_owned(),
        ));
    }
    let cut = inherited_event_count.unwrap_or(0);
    if !header.is_seeded && cut != 0 {
        return Err(PersistError::Corruption(
            "unseeded session header inherited event count must be 0".to_owned(),
        ));
    }
    let mut line = serde_json::Map::new();
    line.insert("type".to_owned(), json!("session"));
    line.insert("version".to_owned(), json!(super::SESSION_FORMAT_VERSION));
    line.insert("id".to_owned(), json!(header.id.as_str()));
    line.insert("createdAt".to_owned(), json!(header.created_at));
    if let Some(cwd) = &header.cwd {
        line.insert("cwd".to_owned(), json!(cwd));
    }
    if let Some(parent) = &header.parent_session {
        line.insert("parentSession".to_owned(), json!(parent.as_str()));
    }
    line.insert("isSeeded".to_owned(), json!(header.is_seeded));
    if let Some(origin) = header.origin {
        let Origin::Subagent = origin;
        line.insert("origin".to_owned(), json!("subagent"));
    }
    line.insert(
        "delegationDepth".to_owned(),
        json!(header.delegation_depth.unwrap_or(0)),
    );
    if let Some(preset) = &header.agent_preset {
        line.insert("agentPreset".to_owned(), json!(preset));
    }
    Ok(Value::Object(line))
}

/// Refuses policy fields that never belong to a released session header
/// (dsh `assertNoRetiredHeaderFields`).
///
/// # Errors
///
/// Returns `Corruption` with the dsh message when a retired field is
/// present.
pub fn assert_no_retired_header_fields(value: &Value) -> Result<(), PersistError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    if object.contains_key("sandboxMode") || object.contains_key("approvalPolicy") {
        return Err(PersistError::Corruption(
            "session header uses retired policy baseline fields".to_owned(),
        ));
    }
    Ok(())
}

/// Refuses a header carrying a format version this build does not read
/// BEFORE validating the current header shape or decoding any event row
///
/// (dsh `refuseForeignFormatVersion`): a future format need not satisfy
/// this build's structural checks at all.
///
/// # Errors
///
/// Returns `Unsupported` with the dsh `sessionFormatVersionRefusal` text.
pub fn refuse_foreign_format_version(parsed: &Value) -> Result<(), PersistError> {
    let Some(version) = parsed.get("version").and_then(Value::as_u64) else {
        return Ok(());
    };
    if version == super::SESSION_FORMAT_VERSION {
        return Ok(());
    }
    let id = match parsed.get("id") {
        Some(Value::String(id)) => id.clone(),
        other => other.cloned().unwrap_or(Value::Null).to_string(),
    };
    Err(PersistError::Unsupported(session_format_version_refusal(
        &id, version,
    )))
}

fn is_safe_nonnegative(value: &Value) -> bool {
    value
        .as_u64()
        .is_some_and(|value| value <= SAFE_INTEGER_MAX)
}

fn is_absolute_posix_text(value: &str) -> bool {
    std::path::Path::new(value).is_absolute()
}

/// Type guard: a parsed first line is a well-formed physical session header
/// (dsh `isHeaderLine`; the version gate has already run).
#[must_use]
pub fn is_header_line(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if HEADER_REQUIRED_KEYS
        .iter()
        .any(|key| !object.contains_key(*key))
    {
        return false;
    }
    if object.keys().any(|key| {
        !HEADER_REQUIRED_KEYS.contains(&key.as_str())
            && !HEADER_OPTIONAL_KEYS.contains(&key.as_str())
    }) {
        return false;
    }
    if object.get("type").and_then(Value::as_str) != Some("session") {
        return false;
    }
    if object.get("version").and_then(Value::as_f64).is_none() {
        return false;
    }
    if object.get("id").and_then(Value::as_str).is_none() {
        return false;
    }
    let Some(created_at) = object.get("createdAt") else {
        return false;
    };
    if !is_safe_nonnegative(created_at) {
        return false;
    }
    let Some(depth) = object.get("delegationDepth") else {
        return false;
    };
    if !is_safe_nonnegative(depth) {
        return false;
    }
    // Optional keys distinguish "absent" from "present": a JSON `null`
    // value is present and therefore rejected, exactly like the dsh
    // `=== undefined` guards.
    if let Some(cwd) = object.get("cwd") {
        let Some(cwd) = cwd.as_str() else {
            return false;
        };
        if !is_absolute_posix_text(cwd) {
            return false;
        }
    }
    for key in ["parentSession", "agentPreset"] {
        if let Some(value) = object.get(key)
            && !value.is_string()
        {
            return false;
        }
    }
    if object.get("isSeeded").and_then(Value::as_bool).is_none() {
        return false;
    }
    if let Some(origin) = object.get("origin")
        && origin.as_str() != Some("subagent")
    {
        return false;
    }
    true
}

/// Projects one shape-checked physical header into the logical metadata.
#[must_use]
pub fn from_header_line(line: &Value) -> SessionHeader {
    let get_string = |key: &str| -> Option<String> {
        line.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
    };
    SessionHeader {
        version: super::SESSION_FORMAT_VERSION,
        id: SessionId::new(get_string("id").unwrap_or_default()),
        created_at: line
            .get("createdAt")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        cwd: get_string("cwd"),
        parent_session: get_string("parentSession").map(SessionId::new),
        is_seeded: line
            .get("isSeeded")
            .and_then(Value::as_bool)
            .unwrap_or_default(),
        origin: get_string("origin").map(|_| Origin::Subagent),
        delegation_depth: line.get("delegationDepth").and_then(Value::as_u64),
        agent_preset: get_string("agentPreset"),
    }
}

/// Parses one complete header record supplied independently from event rows
///
/// (dsh `parseHeaderRecord`): the record must be exactly one
/// newline-terminated line, JSON-parse to a current-version object, and
/// pass the retired-field and shape guards.
///
/// # Errors
///
/// Returns `Corruption` with the dsh message for every structural failure
/// and `Unsupported` for a foreign format version.
pub fn parse_header_record(record: &[u8]) -> Result<SessionHeader, PersistError> {
    if record.is_empty()
        || record.last() != Some(&0x0A)
        || record.iter().position(|byte| *byte == 0x0A) != Some(record.len() - 1)
    {
        return Err(PersistError::Corruption(
            "empty or header-less session log".to_owned(),
        ));
    }
    let parsed: Value = serde_json::from_slice(&record[..record.len() - 1]).map_err(|_error| {
        PersistError::Corruption("corrupt session log: header line is not valid JSON".to_owned())
    })?;
    if !parsed.is_object() {
        return Err(PersistError::Corruption(
            "corrupt session log: first line is not a JSON object".to_owned(),
        ));
    }
    refuse_foreign_format_version(&parsed)?;
    assert_no_retired_header_fields(&parsed)?;
    if !is_header_line(&parsed) {
        return Err(PersistError::Corruption(
            "corrupt session log: first line is not a session header".to_owned(),
        ));
    }
    Ok(from_header_line(&parsed))
}

/// Builds the logical header value (no physical `type` key) for artifact
/// validation (dsh `SessionFormatHeader` shape).
#[must_use]
pub fn logical_header_value(header: &SessionHeader) -> Value {
    let mut value = serde_json::Map::new();
    value.insert("version".to_owned(), json!(super::SESSION_FORMAT_VERSION));
    value.insert("id".to_owned(), json!(header.id.as_str()));
    value.insert("createdAt".to_owned(), json!(header.created_at));
    if let Some(cwd) = &header.cwd {
        value.insert("cwd".to_owned(), json!(cwd));
    }
    if let Some(parent) = &header.parent_session {
        value.insert("parentSession".to_owned(), json!(parent.as_str()));
    }
    value.insert("isSeeded".to_owned(), json!(header.is_seeded));
    if header.origin.is_some() {
        value.insert("origin".to_owned(), json!("subagent"));
    }
    value.insert(
        "delegationDepth".to_owned(),
        json!(header.delegation_depth.unwrap_or(0)),
    );
    if let Some(preset) = &header.agent_preset {
        value.insert("agentPreset".to_owned(), json!(preset));
    }
    Value::Object(value)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        assert_no_retired_header_fields, from_header_line, is_header_line, logical_header_value,
        parse_header_record, refuse_foreign_format_version, to_header_line,
    };
    use crate::persist::PersistError;
    use fr_wire::event::{Origin, SessionHeader};
    use fr_wire::ids::{SessionId, SessionSeqRangeError};

    fn header() -> SessionHeader {
        SessionHeader {
            version: 4,
            id: SessionId::new("s-1"),
            created_at: 1_000,
            cwd: Some("/repo".to_owned()),
            parent_session: None,
            is_seeded: false,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    #[test]
    fn header_line_serializes_in_dsh_key_order() {
        let line = to_header_line(&header(), None).expect("line");
        let text = line.to_string();
        // The dsh encodeHeader insertion order, byte-for-byte.
        assert_eq!(
            text,
            "{\"type\":\"session\",\"version\":4,\"id\":\"s-1\",\"createdAt\":1000,\
\"cwd\":\"/repo\",\"isSeeded\":false,\"delegationDepth\":0}"
        );
    }

    #[test]
    fn header_line_validates_seed_cut_and_defaults_depth() {
        let seeded = SessionHeader {
            is_seeded: true,
            ..header()
        };
        let error = to_header_line(&seeded, None).expect_err("seeded requires cut");
        assert_eq!(
            error.message(),
            "seeded session header requires an inherited event count"
        );
        let error = to_header_line(&header(), Some(7)).expect_err("unseeded cut zero");
        assert_eq!(
            error.message(),
            "unseeded session header inherited event count must be 0"
        );
        let line = to_header_line(&seeded, Some(5)).expect("seeded line");
        assert_eq!(line["delegationDepth"], json!(0));
    }

    #[test]
    fn header_line_guards_reject_malformed_first_records() {
        let error = parse_header_record(b"").expect_err("empty");
        assert_eq!(error.message(), "empty or header-less session log");
        let error = parse_header_record(b"{}").expect_err("no newline");
        assert_eq!(error.message(), "empty or header-less session log");
        let error = parse_header_record(b"line1\nline2\n").expect_err("two lines");
        assert_eq!(error.message(), "empty or header-less session log");
        let error = parse_header_record(b"not json\n").expect_err("bad json");
        assert_eq!(
            error.message(),
            "corrupt session log: header line is not valid JSON"
        );
        let error = parse_header_record(b"[1]\n").expect_err("array");
        assert_eq!(
            error.message(),
            "corrupt session log: first line is not a JSON object"
        );
        let error = parse_header_record(b"{}\n").expect_err("not a header");
        assert_eq!(
            error.message(),
            "corrupt session log: first line is not a session header"
        );
    }

    #[test]
    fn foreign_versions_refuse_before_shape_checks() {
        let future = json!({"type": "session", "version": 5, "id": "s"});
        let error = refuse_foreign_format_version(&future).expect_err("future");
        assert!(matches!(error, PersistError::Unsupported(_)));
        assert!(
            error
                .message()
                .contains("uses log format v5, but this harness reads only v4")
        );
        let older = json!({"type": "session", "version": 3, "id": "s"});
        let error = refuse_foreign_format_version(&older).expect_err("older");
        assert!(
            error
                .message()
                .contains("older than the supported v4, and this build ships no upgrade path")
        );
        // A non-number version defers to the shape guard.
        refuse_foreign_format_version(&json!({"version": "x"})).expect("deferred");
        let _ = SessionSeqRangeError(0);
    }

    #[test]
    fn retired_policy_fields_refuse() {
        let error =
            assert_no_retired_header_fields(&json!({"sandboxMode": "x"})).expect_err("sandboxMode");
        assert_eq!(
            error.message(),
            "session header uses retired policy baseline fields"
        );
        assert_no_retired_header_fields(&json!({"cwd": "/x"})).expect("clean");
    }

    #[test]
    fn header_shape_guard_covers_the_whole_table() {
        let base = json!({
            "type": "session", "version": 4, "id": "s", "createdAt": 1000,
            "isSeeded": false, "delegationDepth": 0
        });
        let with = |key: &str, value: serde_json::Value| {
            let mut object = base.as_object().expect("object").clone();
            object.insert(key.to_owned(), value);
            Value::Object(object)
        };
        assert!(is_header_line(&base));
        // Optional cwd must be absolute when present; a null value is
        // present and therefore rejected.
        assert!(!is_header_line(&with("cwd", json!(null))));
        assert!(!is_header_line(&with("cwd", json!("relative"))));
        assert!(is_header_line(&with("cwd", json!("/abs"))));
        assert!(!is_header_line(&with("origin", json!("other"))));
        assert!(is_header_line(&with("origin", json!("subagent"))));
        assert!(!is_header_line(&with("extra", json!(1))));
        // seedLength is not part of the whitelist: a header carrying it
        // refuses (PRD 7.2 explicit refusal).
        assert!(!is_header_line(&with("seedLength", json!(4))));
        assert!(!is_header_line(&json!({"createdAt": 1.5})));
        assert!(!is_header_line(&json!({"createdAt": -1})));
    }

    #[test]
    fn header_round_trips_through_the_logical_shape() {
        let mut seeded = header();
        seeded.is_seeded = true;
        seeded.origin = Some(Origin::Subagent);
        seeded.delegation_depth = Some(2);
        seeded.agent_preset = Some("preset".to_owned());
        let line = to_header_line(&seeded, Some(4)).expect("line");
        assert!(is_header_line(&line));
        let restored = from_header_line(&line);
        assert_eq!(restored, seeded);
        assert_eq!(
            logical_header_value(&restored),
            json!({
                "version": 4, "id": "s-1", "createdAt": 1000, "cwd": "/repo",
                "isSeeded": true, "origin": "subagent", "delegationDepth": 2,
                "agentPreset": "preset"
            })
        );
    }
}

// Rust guideline compliant 2026-10-23
