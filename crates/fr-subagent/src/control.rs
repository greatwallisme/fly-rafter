// Rust guideline compliant 2026-11-06
//! The client-safe listing vocabulary (dsh `subagent/src/control-types.ts`
//! `SubagentListEntry` and `tool-subagent-control/list-agents.ts` 1:1):
//! the model-facing row projection, the scope resolution, and the
//! verbatim render lines.

use serde_json::{Value, json};

use crate::catalog::CatalogEntry;

/// The listing scope (dsh `ListAgentsScope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Direct children only (the default).
    Children,
    /// The whole tree below the caller.
    Descendants,
}

impl Scope {
    /// Resolves the optional model request (dsh
    /// `resolveListAgentsRequest`).
    #[must_use]
    pub fn resolve(request: &Value) -> Self {
        match request.get("scope").and_then(Value::as_str) {
            Some("descendants") => Self::Descendants,
            _ => Self::Children,
        }
    }
}

/// One model-facing child row (one-shot and external children are
/// filtered out: they cannot be continued, so the model never selects
/// them — dsh `project`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRow {
    /// The durable child session id.
    pub id: String,
    /// The durable creation label.
    pub label: String,
    /// Turn activity without exposing whether the child is loaded.
    pub status: &'static str,
    /// The catalog parent (descendants scope only).
    pub parent: Option<String>,
    /// The edge distance from the requested root (descendants only).
    pub depth: Option<u64>,
}

/// One branch diagnostic row (dsh `SubagentListEntry` diagnostic arm).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRow {
    /// The candidate's session id.
    pub id: String,
    /// `corrupt`: invalid or conflicting session data; `unavailable`:
    /// could not be read; `unsupported`: unknown catalog mode (its
    /// readable catalog is still traversed).
    pub reason: &'static str,
    /// The catalog parent (descendants scope only).
    pub parent: Option<String>,
    /// The edge distance from the requested root (descendants only).
    pub depth: Option<u64>,
}

/// The projected row union served to the tool output schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A continuable child row.
    Child(ChildRow),
    /// A branch diagnostic row.
    Diagnostic(DiagnosticRow),
}

/// Projects one direct-child catalog entry, or omits it (dsh `project`).
#[must_use]
pub fn project_child(
    entry: &CatalogEntry,
    status: &'static str,
    position: Option<(&str, u64)>,
) -> Option<Row> {
    if entry.mode != "continuable" {
        return None;
    }
    let (parent, depth) = match position {
        Some((parent, depth)) => (Some(parent.to_owned()), Some(depth)),
        None => (None, None),
    };
    Some(Row::Child(ChildRow {
        id: entry.id.clone(),
        label: entry.label.clone().unwrap_or_default(),
        status,
        parent,
        depth,
    }))
}

/// Renders the verbatim list text (dsh `list-agents.ts` `render`): the
/// empty state, the `<id> [<status>] — <label>` children line, and the
/// descendants position suffix.
#[must_use]
pub fn render(rows: &[Row], scope: Scope) -> String {
    if rows.is_empty() {
        return "(no subagents)".to_owned();
    }
    rows.iter()
        .map(|row| match row {
            Row::Diagnostic(diagnostic) => {
                let at = position_suffix(scope, diagnostic.parent.as_deref(), diagnostic.depth);
                format!(
                    "{} [diagnostic: {}]{}",
                    diagnostic.id, diagnostic.reason, at
                )
            }
            Row::Child(child) => {
                let at = position_suffix(scope, child.parent.as_deref(), child.depth);
                format!("{} [{}]{} — {}", child.id, child.status, at, child.label)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The descendants-only position suffix (dsh `at` in the render arm).
fn position_suffix(scope: Scope, parent: Option<&str>, depth: Option<u64>) -> String {
    match scope {
        Scope::Children => String::new(),
        Scope::Descendants => format!(
            " parent={} depth={}",
            parent.unwrap_or_default(),
            depth.map_or_else(String::new, |depth| depth.to_string()),
        ),
    }
}

/// The tool output schema row for one projected entry (the dsh output
/// `oneOf` pair, wire form).
#[must_use]
pub fn output_row(row: &Row) -> Value {
    match row {
        Row::Diagnostic(diagnostic) => {
            let mut value = json!({
                "kind": "diagnostic",
                "id": diagnostic.id,
                "reason": diagnostic.reason,
            });
            if let (Some(parent), Some(depth)) = (&diagnostic.parent, diagnostic.depth) {
                value["parent"] = json!(parent);
                value["depth"] = json!(depth);
            }
            value
        }
        Row::Child(child) => {
            let mut value = json!({
                "kind": "child",
                "id": child.id,
                "label": child.label,
                "status": child.status,
            });
            if let (Some(parent), Some(depth)) = (&child.parent, child.depth) {
                value["parent"] = json!(parent);
                value["depth"] = json!(depth);
            }
            value
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CatalogEntry, Row, Scope, project_child, render};
    use serde_json::json;

    fn entry(id: &str, mode: &str, label: Option<&str>) -> CatalogEntry {
        CatalogEntry {
            id: id.to_owned(),
            created_at: 0,
            mode: mode.to_owned(),
            label: label.map(str::to_owned),
        }
    }

    #[test]
    fn one_shot_and_external_children_are_omitted() {
        assert!(project_child(&entry("a", "one-shot", Some("x")), "inactive", None).is_none());
        assert!(project_child(&entry("b", "external", Some("x")), "inactive", None).is_none());
        let row = project_child(
            &entry("c", "continuable", Some("research")),
            "running",
            None,
        )
        .expect("the continuable row");
        let Row::Child(child) = row else {
            panic!("child");
        };
        assert_eq!(child.id, "c");
        assert_eq!(child.status, "running");
    }

    #[test]
    fn the_render_lines_match_the_dsh_shapes_verbatim() {
        assert_eq!(render(&[], Scope::Children), "(no subagents)");
        let rows = [
            project_child(
                &entry("c1", "continuable", Some("research")),
                "running",
                None,
            )
            .expect("row"),
            project_child(
                &entry("c2", "continuable", Some("review")),
                "inactive",
                None,
            )
            .expect("row"),
        ];
        assert_eq!(
            render(&rows, Scope::Children),
            "c1 [running] — research\nc2 [inactive] — review"
        );
    }

    #[test]
    fn the_descendants_render_carries_the_position_suffix() {
        let row = project_child(
            &entry("grand", "continuable", Some("deep work")),
            "inactive",
            Some(("c1", 2)),
        )
        .expect("row");
        assert_eq!(
            render(&[row], Scope::Descendants),
            "grand [inactive] parent=c1 depth=2 — deep work"
        );
        let diagnostic = Row::Diagnostic(super::DiagnosticRow {
            id: "broken".to_owned(),
            reason: "corrupt",
            parent: Some("c1".to_owned()),
            depth: Some(3),
        });
        assert_eq!(
            render(&[diagnostic], Scope::Descendants),
            "broken [diagnostic: corrupt] parent=c1 depth=3"
        );
    }

    #[test]
    fn the_scope_resolution_defaults_to_children() {
        assert_eq!(Scope::resolve(&json!({})), Scope::Children);
        assert_eq!(
            Scope::resolve(&json!({ "scope": "descendants" })),
            Scope::Descendants
        );
        assert_eq!(
            Scope::resolve(&json!({ "scope": "mystery" })),
            Scope::Children
        );
    }
}
