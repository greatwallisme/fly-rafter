// Rust guideline compliant 2026-10-28
//! Configured tool ordering (dsh system-prompt/src/index.ts
//! `validateToolOrder`/`orderTools`, verbatim semantics).
//!
//! The reserved `<unlisted-tools>` marker must appear exactly once in a
//! configured order; unlisted tools insert there lexicographically; a
//! configured name that no provider knows fails assembly; a provider that
//! returns the reserved name fails outright.

use std::cmp::Ordering;
use std::collections::HashSet;

use fr_wire::call_config::ToolSchema;

use crate::interpolate::PromptError;
use crate::prompts::TOOL_ORDER_REST;

/// Code-unit name comparison — locale-independent, so the order is
/// identical on every machine (dsh `compareNames` compares UTF-16 code
/// units; `String` byte order equals it on the ASCII tool-name domain and
/// diverges only outside it).
fn compare_names(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Validate duplicate names and the required rest marker (dsh
/// `validateToolOrder`); registered names are checked later at assembly
/// because plugins have not loaded yet.
///
/// # Errors
///
/// Returns the verbatim dsh error for a duplicate or missing rest entry.
pub fn validate_tool_order(tool_order: &[String]) -> Result<(), PromptError> {
    let mut seen = HashSet::new();
    for name in tool_order {
        if !seen.insert(name.as_str()) {
            return Err(PromptError::new(format!(
                "toolOrder lists \"{name}\" more than once"
            )));
        }
    }
    if !seen.contains(TOOL_ORDER_REST) {
        return Err(PromptError::new(format!(
            "toolOrder must contain the \"{TOOL_ORDER_REST}\" rest entry (where unlisted tools are inserted)"
        )));
    }
    Ok(())
}

/// Apply configured tool order, inserting unlisted tools lexicographically
/// at the rest entry (dsh `orderTools`). Unknown configured names fail;
/// known names absent from one assembly are a normal absence.
///
/// # Errors
///
/// Returns the verbatim dsh error for a reserved provider name or an
/// unregistered configured name.
pub fn order_tools<S: std::hash::BuildHasher>(
    tools: &[ToolSchema],
    tool_order: Option<&[String]>,
    known_names: &HashSet<String, S>,
) -> Result<Vec<ToolSchema>, PromptError> {
    if tools.iter().any(|tool| tool.name == TOOL_ORDER_REST) {
        return Err(PromptError::new(format!(
            "tool provider returned reserved tool name \"{TOOL_ORDER_REST}\" (reserved for toolOrder's rest entry)"
        )));
    }
    let Some(tool_order) = tool_order else {
        let mut ordered = tools.to_vec();
        ordered.sort_by(|a, b| compare_names(&a.name, &b.name));
        return Ok(ordered);
    };
    let unknown: Vec<&str> = tool_order
        .iter()
        .map(String::as_str)
        .filter(|name| *name != TOOL_ORDER_REST && !known_names.contains(*name))
        .collect();
    if !unknown.is_empty() {
        let mut known: Vec<&str> = known_names.iter().map(String::as_str).collect();
        known.sort_unstable();
        let unknown_list = unknown
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let known_list = if known.is_empty() {
            "(none)".to_string()
        } else {
            known.join(", ")
        };
        return Err(PromptError::new(format!(
            "toolOrder lists unregistered tool{} {unknown_list}; known tools: {known_list}",
            if unknown.len() > 1 { "s" } else { "" },
        )));
    }
    let listed: HashSet<&str> = tool_order.iter().map(String::as_str).collect();
    let mut rest: Vec<ToolSchema> = tools
        .iter()
        .filter(|tool| !listed.contains(tool.name.as_str()))
        .cloned()
        .collect();
    rest.sort_by(|a, b| compare_names(&a.name, &b.name));
    let mut ordered = Vec::with_capacity(tools.len());
    for name in tool_order {
        if name == TOOL_ORDER_REST {
            ordered.append(&mut rest);
        } else {
            ordered.extend(tools.iter().filter(|tool| tool.name == *name).cloned());
        }
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            defer_loading: None,
            name: name.to_string(),
            description: format!("tool {name}"),
            parameters: serde_json::Map::new(),
        }
    }

    fn tool_with_description(name: &str, description: &str) -> ToolSchema {
        ToolSchema {
            defer_loading: None,
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::Map::new(),
        }
    }

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn rest_marker_is_the_dsh_literal() {
        assert_eq!(TOOL_ORDER_REST, "<unlisted-tools>");
    }

    #[test]
    fn absent_order_sorts_lexicographically() {
        let tools = vec![tool("c"), tool("a"), tool("b")];
        let ordered = order_tools(&tools, None, &HashSet::new()).unwrap();
        let names: Vec<&str> = ordered.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn configured_order_inserts_rest_lexicographically() {
        let tools = vec![tool("z"), tool("alpha"), tool("beta")];
        let order = vec![
            "z".to_string(),
            TOOL_ORDER_REST.to_string(),
            "beta".to_string(),
        ];
        let ordered = order_tools(&tools, Some(&order), &known(&["z", "beta", "alpha"])).unwrap();
        let names: Vec<&str> = ordered.iter().map(|t| t.name.as_str()).collect();
        // alpha is unlisted and lands at the rest entry in name order.
        assert_eq!(names, ["z", "alpha", "beta"]);
    }

    #[test]
    fn duplicate_configured_name_fails() {
        let order = vec![
            "a".to_string(),
            "a".to_string(),
            TOOL_ORDER_REST.to_string(),
        ];
        assert_eq!(
            validate_tool_order(&order).unwrap_err().0,
            "toolOrder lists \"a\" more than once"
        );
    }

    #[test]
    fn missing_rest_entry_fails() {
        let order = vec!["a".to_string()];
        assert_eq!(
            validate_tool_order(&order).unwrap_err().0,
            "toolOrder must contain the \"<unlisted-tools>\" rest entry (where unlisted tools are inserted)"
        );
    }

    #[test]
    fn unknown_configured_name_names_every_tool() {
        let tools = vec![tool("a")];
        let order = vec![
            "a".to_string(),
            "typo".to_string(),
            TOOL_ORDER_REST.to_string(),
        ];
        let error = order_tools(&tools, Some(&order), &known(&["a"])).unwrap_err();
        assert_eq!(
            error.0,
            "toolOrder lists unregistered tool \"typo\"; known tools: a"
        );
    }

    #[test]
    fn unknown_names_pluralize_and_list_all_known() {
        let order = vec![
            "x".to_string(),
            "y".to_string(),
            TOOL_ORDER_REST.to_string(),
        ];
        let error = order_tools(&[], Some(&order), &known(&["b", "a"])).unwrap_err();
        assert_eq!(
            error.0,
            "toolOrder lists unregistered tools \"x\", \"y\"; known tools: a, b"
        );
    }

    #[test]
    fn provider_returning_the_reserved_name_fails() {
        let tools = vec![tool(TOOL_ORDER_REST)];
        let error = order_tools(&tools, None, &HashSet::new()).unwrap_err();
        assert_eq!(
            error.0,
            "tool provider returned reserved tool name \"<unlisted-tools>\" (reserved for toolOrder's rest entry)"
        );
    }

    #[test]
    fn collection_order_survives_between_equal_names() {
        let tools = vec![
            tool_with_description("dup", "first"),
            tool_with_description("dup", "second"),
        ];
        let ordered = order_tools(&tools, None, &HashSet::new()).unwrap();
        // The stable sort keeps collection order between equal names.
        assert_eq!(ordered[0].description, "first");
        assert_eq!(ordered[1].description, "second");
    }

    #[test]
    fn configured_name_absent_from_this_assembly_is_normal_absence() {
        let tools = vec![tool("a")];
        let order = vec![
            "a".to_string(),
            "restricted".to_string(),
            TOOL_ORDER_REST.to_string(),
        ];
        let ordered = order_tools(&tools, Some(&order), &known(&["a", "restricted"])).unwrap();
        let names: Vec<&str> = ordered.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a"]);
    }
}
