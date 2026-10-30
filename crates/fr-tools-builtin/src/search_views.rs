// Rust guideline compliant 2026-10-30
//! Result-time search-card presentation for `grep` and `glob` (dsh
//! `tool-fs-search/src/presentation.ts`).
//!
//! Both tools land on one search render intent with two
//! shape-discriminated variants: `grep` projects its matches grouped by
//! file, `glob` projects a flat path list. The module owns the
//! value→`presentationMeta` projection each tool declares and the
//! defensive `meta`→view narrowing each tool's `presentResult` reads
//! back on replay.

use serde_json::Value;
use serde_json::json;

use crate::search_core::GrepMatch;

/// Groups flat matches by file (first-seen order) into the structured
/// by-file shape a UI renders as expandable per-file groups (dsh
/// `groupMatchesByFile`).
///
/// The grouping matches the model-facing text grouping, so card and
/// text agree about file order and membership.
///
/// # Panics
///
/// Never in practice: the map slots are inserted before their reads in
/// the same single-threaded pass (the dsh loop-shape expectation).
#[must_use]
pub fn group_matches_by_file(matches: &[GrepMatch]) -> Vec<(String, Vec<Value>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<&GrepMatch>> =
        std::collections::HashMap::new();
    for match_ in matches {
        if !groups.contains_key(&match_.path) {
            order.push(match_.path.clone());
            groups.insert(match_.path.clone(), Vec::new());
        }
        groups
            .get_mut(&match_.path)
            .expect("the group was just created")
            .push(match_);
    }
    order
        .into_iter()
        .map(|path| {
            let group = groups.get(&path).expect("the group was just created");
            (
                path.clone(),
                group
                    .iter()
                    .map(|m| {
                        json!({
                            "lineNumber": m.line_number,
                            "line": m.line,
                        })
                    })
                    .collect(),
            )
        })
        .collect()
}

/// The serialized UTF-8 byte size of one meta payload (the size
/// persisted and re-sent).
fn meta_bytes(meta: &Value) -> usize {
    serde_json::to_string(meta).map_or(usize::MAX, |text| text.len())
}

/// Drops trailing top-level items (file groups or paths) until the
/// serialized meta fits `max_meta_bytes`, marking the result truncated
/// (dsh `capMetaBytes`).
///
/// `total` is preserved; a single item too large to fit on its own is
/// kept: the invariant is a bounded payload wherever droppable, never
/// an empty card that hides a real result.
#[must_use]
pub fn cap_meta_bytes(meta: Value, max_meta_bytes: usize) -> Value {
    if meta_bytes(&meta) <= max_meta_bytes {
        return meta;
    }
    if meta.get("shape").and_then(Value::as_str) == Some("matches") {
        let mut files: Vec<Value> = meta
            .get("files")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        while files.len() > 1 {
            files.pop();
            let candidate = json!({
                "shape": "matches",
                "files": files,
                "truncated": true,
                "total": meta.get("total").cloned().unwrap_or(Value::Null),
            });
            if meta_bytes(&candidate) <= max_meta_bytes {
                return candidate;
            }
        }
        return json!({
            "shape": "matches",
            "files": files,
            "truncated": true,
            "total": meta.get("total").cloned().unwrap_or(Value::Null),
        });
    }
    let mut paths: Vec<Value> = meta
        .get("paths")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    while paths.len() > 1 {
        paths.pop();
        let candidate = json!({
            "shape": "paths",
            "paths": paths,
            "truncated": true,
            "total": meta.get("total").cloned().unwrap_or(Value::Null),
        });
        if meta_bytes(&candidate) <= max_meta_bytes {
            return candidate;
        }
    }
    json!({
        "shape": "paths",
        "paths": paths,
        "truncated": true,
        "total": meta.get("total").cloned().unwrap_or(Value::Null),
    })
}

/// Projects the retained `grep` matches into the search meta (dsh
/// `grepSearchMeta`): grouped by file, `total` (every parsed match) and
/// `truncated`, bounded to the serialized-meta byte budget.
#[must_use]
pub fn grep_search_meta(retained: (&[GrepMatch], bool, usize), max_meta_bytes: usize) -> Value {
    let (items, truncated, seen) = retained;
    let meta = json!({
        "shape": "matches",
        "files": group_matches_by_file(items)
            .into_iter()
            .map(|(path, matches)| json!({ "path": path, "matches": matches }))
            .collect::<Vec<_>>(),
        "truncated": truncated,
        "total": seen,
    });
    cap_meta_bytes(meta, max_meta_bytes)
}

/// Projects the retained `glob` paths into the search meta (dsh
/// `globSearchMeta`): the flat list, `total` and `truncated`, bounded to
/// the serialized-meta byte budget.
#[must_use]
pub fn glob_search_meta(retained: (&[String], bool, usize), max_meta_bytes: usize) -> Value {
    let (items, truncated, seen) = retained;
    let meta = json!({
        "shape": "paths",
        "paths": items,
        "truncated": truncated,
        "total": seen,
    });
    cap_meta_bytes(meta, max_meta_bytes)
}

/// Whether one meta entry is a valid line match (the defensive
/// narrowing; dsh `isSearchLineMatch`).
fn is_line_match(value: &Value) -> bool {
    value.is_object()
        && value.get("lineNumber").is_some_and(Value::is_number)
        && value.get("line").is_some_and(Value::is_string)
}

/// Whether one meta entry is a valid file group (dsh
/// `isSearchFileMatches`).
fn is_file_matches(value: &Value) -> bool {
    value.is_object()
        && value.get("path").is_some_and(Value::is_string)
        && value
            .get("matches")
            .and_then(Value::as_array)
            .is_some_and(|matches| matches.iter().all(is_line_match))
}

/// Narrows opaque live or replayed result metadata to the search view
/// (dsh `searchViewFromMeta`).
///
/// Malformed metadata returns `None` so the presenters fall back to the
/// generic card instead of failing during replay. A zero-result meta
/// narrows to a valid empty card — a zero-match grep is a legitimate
/// result a UI shows as "no matches", not an absent projection.
#[must_use]
pub fn search_view_from_meta(meta: Option<&Value>) -> Option<Value> {
    let meta = meta?;
    if !meta.is_object() {
        return None;
    }
    let truncated = meta.get("truncated").and_then(Value::as_bool)?;
    let total = meta.get("total").and_then(Value::as_f64)?;
    if meta.get("shape").and_then(Value::as_str) == Some("matches") {
        let files = meta.get("files").and_then(Value::as_array)?;
        if !files.iter().all(is_file_matches) {
            return None;
        }
        return Some(json!({
            "card": "search",
            "shape": "matches",
            "files": files,
            "truncated": truncated,
            "total": total,
        }));
    }
    if meta.get("shape").and_then(Value::as_str) == Some("paths") {
        let paths = meta.get("paths").and_then(Value::as_array)?;
        if !paths.iter().all(Value::is_string) {
            return None;
        }
        return Some(json!({
            "card": "search",
            "shape": "paths",
            "paths": paths,
            "truncated": truncated,
            "total": total,
        }));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches_meta() -> Value {
        json!({
            "shape": "matches",
            "files": [{ "path": "a.rs", "matches": [{ "lineNumber": 1, "line": "x" }] }],
            "truncated": false,
            "total": 1,
        })
    }

    /// The projections carry the shape, the grouped items, and the
    /// budget facts.
    #[test]
    fn projections_pin_the_shapes() {
        let matches = vec![GrepMatch {
            path: "a.rs".to_owned(),
            line_number: 1,
            line: "x".to_owned(),
        }];
        let meta = grep_search_meta((&matches, false, 1), 1000);
        assert_eq!(meta["shape"], "matches");
        assert_eq!(meta["files"][0]["path"], "a.rs");
        assert_eq!(meta["total"], 1);
        let paths = vec!["a.rs".to_owned(), "b.rs".to_owned()];
        let meta = glob_search_meta((&paths, true, 2), 1000);
        assert_eq!(meta["shape"], "paths");
        assert_eq!(meta["truncated"], true);
        assert_eq!(meta["total"], 2);
    }

    /// The byte cap drops trailing items and marks the result, keeping
    /// at least one.
    #[test]
    fn cap_meta_bytes_drops_trailing_and_keeps_one() {
        let paths: Vec<String> = (0..50)
            .map(|index| format!("very-long-path-{index}.rs"))
            .collect();
        let meta = json!({
            "shape": "paths",
            "paths": paths,
            "truncated": false,
            "total": 50,
        });
        let capped = cap_meta_bytes(meta, 300);
        assert_eq!(capped["truncated"], true);
        assert_eq!(capped["total"], 50);
        let kept = capped["paths"].as_array().expect("paths").len();
        assert!(kept > 0 && kept < 50, "{kept}");
        let single = cap_meta_bytes(matches_meta(), 10);
        assert_eq!(single["files"].as_array().expect("files").len(), 1);
        assert_eq!(single["truncated"], true);
    }

    /// The narrowing accepts well-formed metas, the empty card, and
    /// declines malformed ones.
    #[test]
    fn narrowing_accepts_the_well_formed_and_declines_the_rest() {
        let view = search_view_from_meta(Some(&matches_meta())).expect("view");
        assert_eq!(view["card"], "search");
        assert_eq!(view["shape"], "matches");
        let empty = json!({
            "shape": "matches",
            "files": [],
            "truncated": false,
            "total": 0,
        });
        assert!(search_view_from_meta(Some(&empty)).is_some());
        assert!(search_view_from_meta(None).is_none());
        assert!(search_view_from_meta(Some(&json!("text"))).is_none());
        let broken = json!({
            "shape": "matches",
            "files": [{ "path": "a.rs", "matches": [{ "lineNumber": "x", "line": "y" }] }],
            "truncated": false,
            "total": 1,
        });
        assert!(search_view_from_meta(Some(&broken)).is_none());
        let missing = json!({ "shape": "other" });
        assert!(search_view_from_meta(Some(&missing)).is_none());
    }
}
