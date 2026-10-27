// Rust guideline compliant 2026-10-28
//! Strict `{{variable}}` interpolation and the render family (dsh
//! system-prompt/src/index.ts `interpolate`/`renderPrompt`/
//! `renderContextSections`/`joinContextSections`, verbatim semantics).
//!
//! References are complete simple groups: a lone `{{` with no later `}}`
//! is literal prose, anything between that is not a well-formed
//! `^[a-z][a-z0-9_]*$` name is a malformed reference, unregistered or
//! valueless names throw naming their section, and substituted values are
//! never scanned again.

use std::fmt;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::prompts::RUNTIME_CONTEXT_SNAPSHOT_TEMPLATE;

/// One failed interpolation or render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptError(pub String);

impl PromptError {
    /// Wraps one verbatim dsh error message.
    #[must_use]
    pub const fn new(message: String) -> Self {
        Self(message)
    }
}

impl fmt::Display for PromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PromptError {}

/// The variable shape carried by an assembly.
///
/// Registration order, with `None` marking a registered name without a
/// value for this assembly (dsh `Record<string, string | undefined>`; over
/// the wire a valueless key folds away, which is lossless because only
/// assembly re-renders it).
pub type Variables = IndexMap<String, Option<String>>;

/// One section of an assembly with its text resolved (dsh
/// `AssembledSection`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssembledSection {
    /// The contributing section's unique name.
    pub name: String,
    /// The resolved (but not yet interpolated) section text.
    pub text: String,
    /// Whether to interpolate prompt variables; absent means true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpolate: Option<bool>,
}

/// One resolved dynamic context contribution (dsh `AssembledContext`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssembledContext {
    /// The contributing context's unique name.
    pub name: String,
    /// The resolved text before variable interpolation.
    pub text: String,
    /// Whether to interpolate prompt variables; absent means true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpolate: Option<bool>,
}

/// One attributed runtime-context contribution as the model sees it (dsh
/// `ContextSnapshotSection`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshotSection {
    /// The contributing context's unique name.
    pub name: String,
    /// The rendered context text.
    pub text: String,
}

/// A merge-extensible assembled model input (dsh `PromptAssembly`);
/// sections and contexts stay uninterpolated until rendered.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptAssembly {
    /// Sections in canonical order.
    pub sections: Vec<AssembledSection>,
    /// Contexts in canonical order.
    pub contexts: Vec<AssembledContext>,
    /// Tool schemas already in canonical order.
    pub tools: Vec<fr_wire::call_config::ToolSchema>,
    /// Prompt variables in registration order.
    pub variables: Variables,
}

/// Whether one input interpolates; absent means true (dsh
/// `interpolate === false` opt-out).
fn interpolates(interpolate: Option<bool>) -> bool {
    interpolate != Some(false)
}

/// One interpolation input: the shared section/context shape with its
/// owning name for diagnostics. Callers branch on `interpolate == false`
/// before reaching here.
struct InterpolationInput<'a> {
    name: &'a str,
    text: &'a str,
}

/// Interpolate one section or context and attribute diagnostics to its
/// owner (dsh `interpolate`, verbatim messages).
fn interpolate_input(
    input: &InterpolationInput<'_>,
    variables: &Variables,
    kind: &str,
) -> Result<String, PromptError> {
    let text = input.text;
    let mut result = String::new();
    let mut last = 0;
    while let Some(relative) = text[last..].find("{{") {
        let open = last + relative;
        let tail = &text[open..];
        if let Some(name) = complete_group(tail) {
            if !valid_variable_name(name) {
                return Err(PromptError::new(format!(
                    "malformed prompt variable reference \"{{{{{name}}}}}\" in {kind} \"{}\" (variable names match /^[a-z][a-z0-9_]*$/)",
                    input.name
                )));
            }
            let Some(value) = variables.get(name) else {
                let known: Vec<&str> = variables.keys().map(String::as_str).collect();
                return Err(PromptError::new(format!(
                    "unknown prompt variable \"{{{{{name}}}}}\" in {kind} \"{}\"; registered variables: {}",
                    input.name,
                    if known.is_empty() {
                        "(none)".to_string()
                    } else {
                        known.join(", ")
                    }
                )));
            };
            let Some(value) = value else {
                return Err(PromptError::new(format!(
                    "prompt variable \"{{{{{name}}}}}\" has no value for this assembly ({kind} \"{}\")",
                    input.name
                )));
            };
            result.push_str(&text[last..open]);
            result.push_str(value);
            // The substituted value is never scanned again.
            last = open + name.len() + 4;
        } else if tail.contains("}}") {
            return Err(PromptError::new(format!(
                "malformed prompt variable reference at \"{}…\" in {kind} \"{}\" (references are complete simple {{{{name}}}} groups)",
                utf16_prefix(tail, 16),
                input.name
            )));
        } else {
            // A lone `{{` without any later `}}` is literal prose.
            result.push_str(&text[last..open + 2]);
            last = open + 2;
        }
    }
    result.push_str(&text[last..]);
    Ok(result)
}

/// The name inside a complete `{{name}}` group at the scan position, when
/// one is there (dsh `GROUP_AT` = `/^\{\{([^{}]*)\}\}/`).
fn complete_group(tail: &str) -> Option<&str> {
    let inner_end = tail[2..].find("}}")? + 2;
    let name = &tail[2..inner_end];
    // `[^{}]*`: an inner brace means this is not a complete simple group.
    if name.contains('{') || name.contains('}') {
        return None;
    }
    Some(name)
}

/// `^[a-z][a-z0-9_]*$` (dsh `VARIABLE_NAME`).
fn valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The first `len` UTF-16 code units of `text`, as a string (dsh
/// `String.prototype.slice` operates on UTF-16 code units).
fn utf16_prefix(text: &str, len: usize) -> String {
    String::from_utf16_lossy(&text.encode_utf16().take(len).collect::<Vec<u16>>())
}

/// Interpolate strict `{{variable}}` references, drop empty sections, and
/// join the rest with blank lines (dsh `renderPrompt`).
///
/// # Errors
///
/// Returns the first malformed, unknown, or valueless reference error.
pub fn render_prompt(assembly: &PromptAssembly) -> Result<String, PromptError> {
    let mut rendered = Vec::with_capacity(assembly.sections.len());
    for section in &assembly.sections {
        let text = if interpolates(section.interpolate) {
            interpolate_input(
                &InterpolationInput {
                    name: &section.name,
                    text: &section.text,
                },
                &assembly.variables,
                "section",
            )?
        } else {
            section.text.clone()
        };
        rendered.push(text);
    }
    rendered.retain(|text| !text.is_empty());
    Ok(rendered.join("\n\n"))
}

/// The attributed runtime-context contributions with variables resolved
/// (dsh `renderContextSections`); empty texts drop out.
///
/// # Errors
///
/// Returns the first interpolation error.
pub fn render_context_sections(
    assembly: &PromptAssembly,
) -> Result<Vec<ContextSnapshotSection>, PromptError> {
    let mut sections = Vec::with_capacity(assembly.contexts.len());
    for context in &assembly.contexts {
        let text = if interpolates(context.interpolate) {
            interpolate_input(
                &InterpolationInput {
                    name: &context.name,
                    text: &context.text,
                },
                &assembly.variables,
                "context",
            )?
        } else {
            context.text.clone()
        };
        sections.push(ContextSnapshotSection {
            name: context.name.clone(),
            text,
        });
    }
    sections.retain(|section| !section.text.is_empty());
    Ok(sections)
}

/// The model-facing snapshot text for an already-rendered section list
/// (dsh `joinContextSections`); an empty body renders to `''`.
#[must_use]
pub fn join_context_sections(sections: &[ContextSnapshotSection]) -> String {
    let body = sections
        .iter()
        .map(|section| section.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if body.is_empty() {
        return String::new();
    }
    RUNTIME_CONTEXT_SNAPSHOT_TEMPLATE.replace("{body}", &body)
}

/// The complete dynamic context snapshot (dsh `renderContextSnapshot`).
///
/// # Errors
///
/// Returns the first interpolation error.
pub fn render_context_snapshot(assembly: &PromptAssembly) -> Result<String, PromptError> {
    Ok(join_context_sections(&render_context_sections(assembly)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assembly(sections: &[(&str, &str)], variables: &[(&str, Option<&str>)]) -> PromptAssembly {
        PromptAssembly {
            sections: sections
                .iter()
                .map(|(name, text)| AssembledSection {
                    name: (*name).to_string(),
                    text: (*text).to_string(),
                    interpolate: None,
                })
                .collect(),
            contexts: Vec::new(),
            tools: Vec::new(),
            variables: variables
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.map(ToString::to_string)))
                .collect(),
        }
    }

    #[test]
    fn interpolates_registered_values_without_rescanning() {
        let assembly = assembly(&[("s", "a {{x}} b")], &[("x", Some("{{x}} literal"))]);
        // The substituted value keeps its own braces verbatim.
        assert_eq!(render_prompt(&assembly).unwrap(), "a {{x}} literal b");
    }

    #[test]
    fn lone_open_brace_without_close_is_literal_prose() {
        let assembly = assembly(&[("s", "oops {{ and more")], &[("x", Some("v"))]);
        assert_eq!(render_prompt(&assembly).unwrap(), "oops {{ and more");
    }

    #[test]
    fn malformed_group_with_later_close_names_the_excerpt() {
        // An inner brace breaks the complete-simple-group shape, and a
        // later `}}` proves the prose meant a reference.
        let assembly = assembly(&[("s", "{{a{b}}c tail")], &[]);
        let error = render_prompt(&assembly).unwrap_err();
        assert!(
            error.0.starts_with(
                "malformed prompt variable reference at \"{{a{b}}c tail…\" in section \"s\""
            ),
            "{error}"
        );
    }

    #[test]
    fn malformed_name_reports_the_name_pattern() {
        let assembly = assembly(&[("s", "{{Bad}}")], &[]);
        assert_eq!(
            render_prompt(&assembly).unwrap_err().0,
            "malformed prompt variable reference \"{{Bad}}\" in section \"s\" (variable names match /^[a-z][a-z0-9_]*$/)"
        );
    }

    #[test]
    fn empty_group_is_malformed() {
        let assembly = assembly(&[("s", "{{}}")], &[]);
        assert!(
            render_prompt(&assembly)
                .unwrap_err()
                .0
                .contains("malformed prompt variable reference \"{{}}\"")
        );
    }

    #[test]
    fn unknown_variable_lists_registered_names() {
        let assembly = assembly(&[("s", "{{nope}}")], &[("a", Some("1")), ("b", Some("2"))]);
        assert_eq!(
            render_prompt(&assembly).unwrap_err().0,
            "unknown prompt variable \"{{nope}}\" in section \"s\"; registered variables: a, b"
        );
    }

    #[test]
    fn unknown_variable_without_registered_names_says_none() {
        let assembly = assembly(&[("s", "{{nope}}")], &[]);
        assert_eq!(
            render_prompt(&assembly).unwrap_err().0,
            "unknown prompt variable \"{{nope}}\" in section \"s\"; registered variables: (none)"
        );
    }

    #[test]
    fn registered_name_without_value_fails_for_this_assembly() {
        let assembly = assembly(&[("s", "{{x}}")], &[("x", None)]);
        assert_eq!(
            render_prompt(&assembly).unwrap_err().0,
            "prompt variable \"{{x}}\" has no value for this assembly (section \"s\")"
        );
    }

    #[test]
    fn interpolate_false_keeps_literal_text() {
        let mut prompt = assembly(&[("s", "{{x}}")], &[("x", Some("v"))]);
        prompt.sections[0].interpolate = Some(false);
        assert_eq!(render_prompt(&prompt).unwrap(), "{{x}}");
    }

    #[test]
    fn empty_sections_drop_and_the_rest_join_with_blank_lines() {
        let assembly = assembly(&[("a", "one"), ("b", ""), ("c", "two")], &[]);
        assert_eq!(render_prompt(&assembly).unwrap(), "one\n\ntwo");
    }

    #[test]
    fn context_sections_render_interpolate_and_join() {
        let mut prompt = assembly(&[], &[("cwd", Some("/w"))]);
        prompt.contexts = vec![
            AssembledContext {
                name: "working".to_string(),
                text: "cwd {{cwd}}".to_string(),
                interpolate: None,
            },
            AssembledContext {
                name: "empty".to_string(),
                text: String::new(),
                interpolate: None,
            },
        ];
        let sections = render_context_sections(&prompt).unwrap();
        assert_eq!(sections.len(), 1, "empty contributions drop");
        assert_eq!(sections[0].text, "cwd /w");
        assert_eq!(
            join_context_sections(&sections),
            "Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\ncwd /w"
        );
    }

    #[test]
    fn empty_context_body_renders_empty() {
        assert_eq!(join_context_sections(&[]), "");
    }

    #[test]
    fn context_errors_name_their_owner() {
        let mut prompt = assembly(&[], &[]);
        prompt.contexts = vec![AssembledContext {
            name: "ctx".to_string(),
            text: "{{missing}}".to_string(),
            interpolate: None,
        }];
        assert_eq!(
            render_context_snapshot(&prompt).unwrap_err().0,
            "unknown prompt variable \"{{missing}}\" in context \"ctx\"; registered variables: (none)"
        );
    }
}
