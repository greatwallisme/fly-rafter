// Rust guideline compliant 2026-11-06
//! The parent-owned durable catalog: the versioned `subagent/catalog`
//! event rows and their chunked-list projection state (dsh
//! `subagent/src/catalog.ts` plus `util/chunked-list` 1:1).
//!
//! The chunked list bounds copying: each append shares the unchanged
//! older chunks and copies at most one 64-value chunk.

use serde_json::{Value, json};

/// One chunk's capacity (dsh `CHUNK_CAPACITY`).
pub const CHUNK_CAPACITY: usize = 64;

/// The catalog payload version a local child creation emits (dsh
/// `SUBAGENT_CATALOG_VERSION`).
pub const CATALOG_VERSION: u64 = 0;
/// The version an external execution record carries.
pub const EXTERNAL_CATALOG_VERSION: u64 = 2;

/// One raw catalog event row (the dsh `SubagentCatalogEvent` union).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogRow {
    /// The durable child session id.
    pub child_id: String,
    /// The child's creation time (milliseconds).
    pub child_created_at: u64,
    /// The discovery mode.
    pub mode: &'static str,
    /// The durable creation label (required for continuable rows).
    pub label: Option<String>,
}

/// The row the projection view serves: identity plus mode and label.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    /// The durable child session id.
    pub id: String,
    /// The child's creation time (milliseconds).
    pub created_at: u64,
    /// The discovery mode.
    pub mode: String,
    /// The durable creation label.
    pub label: Option<String>,
}

/// Appends one row to the persistent chunked list (dsh
/// `appendChunkedList`): at most one chunk copies; older chunks share.
#[must_use]
pub fn append_chunked(head: Option<Value>, value: &Value) -> Value {
    match head {
        None => json!({ "values": [value] }),
        Some(head) => {
            let values = head
                .get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if values.len() >= CHUNK_CAPACITY {
                json!({ "values": [value], "previous": head })
            } else {
                let mut next = values;
                next.push(value.clone());
                let mut chunk = json!({ "values": next });
                if let Some(previous) = head.get("previous") {
                    chunk
                        .as_object_mut()
                        .expect("object")
                        .insert("previous".to_owned(), previous.clone());
                }
                chunk
            }
        }
    }
}

/// Visits every stored value in insertion order (dsh
/// `iterateChunkedList`): O(N) time, O(N / 64) scratch space.
#[must_use]
pub fn iterate_chunked(head: Option<&Value>) -> Vec<Value> {
    let mut chunks: Vec<&Value> = Vec::new();
    let mut current = head;
    while let Some(chunk) = current {
        chunks.push(chunk);
        current = chunk.get("previous");
    }
    let mut values = Vec::new();
    for chunk in chunks.iter().rev() {
        if let Some(items) = chunk.get("values").and_then(Value::as_array) {
            values.extend(items.iter().cloned());
        }
    }
    values
}

/// Validates one catalog row payload (the dsh zod strict schemas).
///
/// # Errors
///
/// Returns the shape refusal for an unknown version, mode, or an
/// extraneous field.
pub fn parse_row(data: &Value) -> Result<CatalogRow, String> {
    let object = data
        .as_object()
        .ok_or_else(|| "subagent/catalog payload must be an object".to_owned())?;
    let version = data
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "subagent/catalog version must be a number".to_owned())?;
    let mode = data
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| "subagent/catalog mode must be a string".to_owned())?;
    let known: &[&str] = match (version, mode) {
        (0 | 1, "one-shot") | (0 | 1, "continuable") => {
            &["version", "childId", "childCreatedAt", "mode", "label"]
        }
        (2, "external") => &["version", "childId", "childCreatedAt", "mode", "label"],
        (1, "unknown") => &["version", "childId", "childCreatedAt", "mode", "label"],
        _ => {
            return Err(format!(
                "subagent/catalog version {version} mode \"{mode}\" is not supported"
            ));
        }
    };
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            return Err(format!(
                "subagent/catalog payload has unknown field \"{key}\""
            ));
        }
    }
    let child_id = data
        .get("childId")
        .and_then(Value::as_str)
        .ok_or_else(|| "subagent/catalog childId must be a string".to_owned())?
        .to_owned();
    let child_created_at = data
        .get("childCreatedAt")
        .and_then(Value::as_u64)
        .ok_or_else(|| "subagent/catalog childCreatedAt must be a number".to_owned())?;
    let label = match data.get("label") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| "subagent/catalog label must be a string".to_owned())?
                .to_owned(),
        ),
    };
    if mode == "continuable" && label.is_none() {
        return Err("subagent/catalog continuable label must be a string".to_owned());
    }
    let mode: &'static str = match mode {
        "one-shot" => "one-shot",
        "continuable" => "continuable",
        "external" => "external",
        "unknown" => "unknown",
        _ => unreachable!("the version/mode match arms cover every surviving mode"),
    };
    Ok(CatalogRow {
        child_id,
        child_created_at,
        mode,
        label,
    })
}

/// Folds the catalog projection state (dsh
/// `subagentCatalogProjectionDefinition`): rows at or above the
/// inherited boundary append to the chunked head; the state keeps the
/// boundary so fork-inherited facts never enter the view.
#[must_use]
pub fn fold(state: Option<Value>, event: &Value, inherited_event_count: u64) -> Value {
    let seq = event.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let boundary = state
        .as_ref()
        .and_then(|value| value.get("inheritedEventCount"))
        .and_then(Value::as_u64)
        .unwrap_or(inherited_event_count);
    if event.get("type").and_then(Value::as_str) != Some(crate::wire::CATALOG_EVENT)
        || seq < boundary
    {
        return state.unwrap_or_else(|| json!({ "inheritedEventCount": boundary }));
    }
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    let head = state.as_ref().and_then(|value| value.get("head").cloned());
    json!({
        "inheritedEventCount": boundary,
        "head": append_chunked(head, &data),
    })
}

/// Materializes the current direct-child rows in catalog event order
/// (dsh `subagentCatalogEntries`).
#[must_use]
pub fn entries(state: Option<&Value>) -> Vec<CatalogEntry> {
    let head = state.and_then(|value| value.get("head"));
    iterate_chunked(head)
        .iter()
        .filter_map(|value| parse_row(value).ok())
        .map(|row| CatalogEntry {
            id: row.child_id,
            created_at: row.child_created_at,
            mode: row.mode.to_owned(),
            label: row.label,
        })
        .collect()
}

/// The local-child catalog fact (dsh `establishCatalogChild`).
#[must_use]
pub fn local_row(child_id: &str, child_created_at: u64, label: &str) -> Value {
    json!({
        "version": CATALOG_VERSION,
        "childId": child_id,
        "childCreatedAt": child_created_at,
        "mode": "continuable",
        "label": label,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CATALOG_VERSION, CHUNK_CAPACITY, CatalogEntry, append_chunked, entries, fold,
        iterate_chunked, local_row, parse_row,
    };
    use serde_json::{Value, json};

    #[test]
    fn the_chunked_list_appends_without_touching_older_chunks() {
        let mut head: Option<Value> = None;
        for index in 0..CHUNK_CAPACITY {
            head = Some(append_chunked(head, &json!(index)));
        }
        let one_chunk = head.clone().expect("head");
        assert_eq!(
            one_chunk["values"].as_array().expect("values").len(),
            CHUNK_CAPACITY
        );
        assert!(one_chunk.get("previous").is_none());
        // The 65th append starts a new chunk linked to the old head.
        head = Some(append_chunked(head, &json!(CHUNK_CAPACITY)));
        let two = head.expect("head");
        assert_eq!(two["values"].as_array().expect("values").len(), 1);
        assert!(two.get("previous").is_some());
        let all = iterate_chunked(Some(&two));
        assert_eq!(all.len(), CHUNK_CAPACITY + 1);
        assert_eq!(all[0], json!(0));
        assert_eq!(all[CHUNK_CAPACITY], json!(CHUNK_CAPACITY));
    }

    #[test]
    fn the_fold_keeps_the_inherited_boundary() {
        let inherited = json!({ "inheritedEventCount": 2 });
        // A pre-boundary catalog event (a fork seed's replayed row) folds away.
        let stale = json!({
            "type": crate::wire::CATALOG_EVENT, "seq": 1,
            "data": local_row("inherited-child", 1, "old"),
        });
        let state = fold(Some(inherited), &stale, 2);
        assert!(state.get("head").is_none());
        let own = json!({
            "type": crate::wire::CATALOG_EVENT, "seq": 4,
            "data": local_row("own-child", 9, "research"),
        });
        let state = fold(Some(state), &own, 2);
        let rows = entries(Some(&state));
        assert_eq!(
            rows,
            [CatalogEntry {
                id: "own-child".to_owned(),
                created_at: 9,
                mode: "continuable".to_owned(),
                label: Some("research".to_owned()),
            }]
        );
    }

    #[test]
    fn the_row_parser_classifies_every_supported_mode() {
        let continuable = local_row("c", 1, "label");
        assert_eq!(continuable["version"], json!(CATALOG_VERSION));
        let row = parse_row(&continuable).expect("continuable");
        assert_eq!(row.mode, "continuable");
        assert_eq!(row.label.as_deref(), Some("label"));

        let external = json!({
            "version": 2, "childId": "e", "childCreatedAt": 2, "mode": "external",
        });
        assert_eq!(parse_row(&external).expect("external").mode, "external");

        let unknown_version = json!({
            "version": 9, "childId": "x", "childCreatedAt": 0, "mode": "continuable",
            "label": "l",
        });
        assert!(parse_row(&unknown_version).is_err());

        let continuable_without_label = json!({
            "version": 0, "childId": "x", "childCreatedAt": 0, "mode": "continuable",
        });
        assert!(parse_row(&continuable_without_label).is_err());

        let extra_field = json!({
            "version": 0, "childId": "x", "childCreatedAt": 0, "mode": "one-shot",
            "mystery": true,
        });
        let error = parse_row(&extra_field).expect_err("the extra field refuses");
        assert!(error.contains("unknown field \"mystery\""), "{error}");
    }
}
