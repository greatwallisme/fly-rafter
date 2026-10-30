//! On-disk JSON unit format: the file is always the current net state,
//! kept human-readable (pretty-printed, stable insertion key order) —
//! that legibility is this backend's reason to exist.
//!
//! One `single`-layout unit is one document with a unit header (the dsh
//! `storage-json` `format.ts`).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::error::{FrStorageError, FrStorageErrorCode};

/// The descriptor's unit name pattern (`^[a-z][a-z0-9_]*$`): safe as a file
/// name and as an identifier segment without escaping.
#[must_use]
pub fn unit_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// In-memory authoritative state of one unit; the file is its projection.
/// `global` is `None` until first written (the dsh `null` "never written"
/// sentinel).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitState {
    /// Unit format version stamped in the header.
    pub version: u32,
    /// Global singleton value; `None` until the first `set`.
    pub global: Option<Value>,
    /// Declared tables in declaration order, records in insertion order.
    pub tables: BTreeMap<String, Map<String, Value>>,
}

/// Serializes a unit state to file content: pretty-printed JSON with a
/// trailing newline.
#[must_use]
pub fn serialize(name: &str, state: &UnitState) -> String {
    let mut document = Map::new();
    let mut header = Map::new();
    header.insert("name".to_owned(), Value::from(name));
    header.insert("version".to_owned(), Value::from(state.version));
    document.insert("unit".to_owned(), Value::Object(header));
    document.insert(
        "global".to_owned(),
        state.global.clone().unwrap_or(Value::Null),
    );
    let mut tables = Map::new();
    for (table, records) in &state.tables {
        tables.insert(table.clone(), Value::Object(records.clone()));
    }
    document.insert("tables".to_owned(), Value::Object(tables));
    let mut body =
        serde_json::to_string_pretty(&Value::Object(document)).unwrap_or_else(|_| "{}".to_owned());
    body.push('\n');
    body
}

/// Parses file content into unit state, validating shape and version.
///
/// # Errors
///
/// Rejects non-JSON content, a non-object document, a missing or foreign
/// unit header, a version mismatch, a non-object tables member, and a
/// non-object record map for a declared table.
pub fn parse(
    text: &str,
    unit_name: &str,
    expected_version: u32,
    tables: &[String],
) -> Result<UnitState, FrStorageError> {
    let foreign = |message: String| {
        FrStorageError::malformed_medium(format!("unit '{unit_name}': {message}"))
    };
    let document: Value = serde_json::from_str(text)
        .map_err(|error| foreign(format!("file is not valid JSON: {error}")))?;
    let Some(document) = document.as_object() else {
        return Err(foreign("file is not a JSON object".to_owned()));
    };
    let Some(header) = document.get("unit").and_then(Value::as_object) else {
        return Err(foreign("missing or foreign unit header".to_owned()));
    };
    let header_ok = header.get("name").and_then(Value::as_str) == Some(unit_name)
        && header.get("version").is_some_and(Value::is_number);
    if !header_ok {
        return Err(foreign("missing or foreign unit header".to_owned()));
    }
    let version = header
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| foreign("missing or foreign unit header".to_owned()))?;
    if version != u64::from(expected_version) {
        return Err(FrStorageError::new(
            FrStorageErrorCode::VersionMismatch,
            format!("unit '{unit_name}': stored version {version} != expected {expected_version}"),
        ));
    }
    let Some(tables_member) = document.get("tables").and_then(Value::as_object) else {
        return Err(foreign("tables is not an object".to_owned()));
    };
    let mut state = UnitState {
        version: expected_version,
        global: document
            .get("global")
            .filter(|value| !value.is_null())
            .cloned(),
        tables: BTreeMap::new(),
    };
    for table in tables {
        match tables_member.get(table) {
            None => {
                state.tables.insert(table.clone(), Map::new());
            }
            Some(records) => {
                let Some(records) = records.as_object() else {
                    return Err(foreign(format!("table '{table}' is not an object")));
                };
                state.tables.insert(table.clone(), records.clone());
            }
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_state() -> UnitState {
        let mut records = Map::new();
        records.insert("k1".to_owned(), json!({"prompt": "hello"}));
        let mut tables = BTreeMap::new();
        tables.insert("tasks".to_owned(), records);
        UnitState {
            version: 1,
            global: None,
            tables,
        }
    }

    #[test]
    fn serialize_then_parse_round_trips() {
        let state = sample_state();
        let text = serialize("schedule", &state);
        assert!(text.ends_with('\n'));
        let parsed =
            parse(&text, "schedule", 1, &["tasks".to_owned()]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed, state);
    }

    #[test]
    fn parse_rejects_foreign_headers_and_mismatched_versions() {
        let text = serialize("schedule", &sample_state());
        assert_eq!(
            parse(&text, "other", 1, &["tasks".to_owned()])
                .unwrap_err()
                .code,
            FrStorageErrorCode::MalformedMedium
        );
        assert_eq!(
            parse(&text, "schedule", 2, &["tasks".to_owned()])
                .unwrap_err()
                .code,
            FrStorageErrorCode::VersionMismatch
        );
        assert_eq!(
            parse("not json", "schedule", 1, &["tasks".to_owned()])
                .unwrap_err()
                .code,
            FrStorageErrorCode::MalformedMedium
        );
        assert_eq!(
            parse("[]", "schedule", 1, &["tasks".to_owned()])
                .unwrap_err()
                .code,
            FrStorageErrorCode::MalformedMedium
        );
        let headerless = json!({"global": null, "tables": {}});
        assert_eq!(
            parse(
                &headerless.to_string(),
                "schedule",
                1,
                &["tasks".to_owned()]
            )
            .unwrap_err()
            .code,
            FrStorageErrorCode::MalformedMedium
        );
        let array_tables =
            json!({"unit": {"name": "schedule", "version": 1}, "global": null, "tables": []});
        assert_eq!(
            parse(
                &array_tables.to_string(),
                "schedule",
                1,
                &["tasks".to_owned()]
            )
            .unwrap_err()
            .code,
            FrStorageErrorCode::MalformedMedium
        );
    }

    #[test]
    fn missing_declared_tables_read_as_empty() {
        let text = serialize("schedule", &UnitState::default());
        let parsed =
            parse(&text, "schedule", 0, &["tasks".to_owned()]).unwrap_or_else(|e| panic!("{e}"));
        assert!(parsed.tables["tasks"].is_empty());
    }

    #[test]
    fn unit_names_follow_the_identifier_pattern() {
        assert!(unit_name_ok("schedule"));
        assert!(unit_name_ok("task_cache_2"));
        assert!(!unit_name_ok("Schedule"));
        assert!(!unit_name_ok("2fast"));
        assert!(!unit_name_ok("with-dash"));
        assert!(!unit_name_ok(""));
    }
}
