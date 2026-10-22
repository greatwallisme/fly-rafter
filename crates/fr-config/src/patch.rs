//! `fr.patch.yml` patch rows, layer composition, and compilation to dougong
//! loader documents.
//!
//! Composition semantics mirror dsh's `applyEntryPatches`
//! (vendor/include/src/index.ts:57-127; 2026-10-22 Q1/Q2 rulings): `id` and
//! `name` are matchers, remaining keys override per key (`config` is replaced
//! wholesale), unmatched ids and mismatched names warn and skip, row order is
//! first-insertion order, and `insert` rows append new root-level rows that
//! later layers can override.

use crate::cond::EvalContext;
use crate::error::{Error, Result};
use crate::source::PathResolver;
use regex::Regex;
use serde_json::Value;
use std::fmt::Write as _;

/// One row of a `fr.patch.yml` document: an entry definition/override, or an
/// `insert` group appending new root-level rows.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchLine {
    /// Row identity (the composition key); a pure override row may omit it
    /// only together with `insert` — an entry row requires an id.
    pub id: Option<String>,
    /// Plugin name; on overrides this is a matcher only.
    pub name: Option<String>,
    /// Plugin configuration; overridden wholesale when present.
    pub config: Option<Value>,
    /// Enablement; overridden when present.
    pub disabled: Option<bool>,
    /// Insert group: append these rows to the assembly root.
    pub insert: Option<Vec<Self>>,
}

const fn row_no_of(index: usize) -> usize {
    index + 1
}

/// Converts one YAML value to JSON, evaluating `!!js` tagged values and
/// `!!js ` prefixed strings through the restricted subset.
fn convert_config(
    value: &serde_yaml_ng::Value,
    ctx: &EvalContext,
    whereami: &str,
) -> Result<Value> {
    match value {
        serde_yaml_ng::Value::Tagged(tagged) => {
            if tagged.tag.to_string().ends_with("js") {
                let text = tagged
                    .value
                    .as_str()
                    .ok_or_else(|| Error::PatchLine {
                        row_no: 0,
                        message: format!("{whereami}: !!js tag must wrap a string expression"),
                    })?
                    .to_owned();
                crate::cond::eval(&text, ctx).map_err(|reason| Error::PatchLine {
                    row_no: 0,
                    message: format!("{whereami}: {reason}"),
                })
            } else {
                Err(Error::PatchLine {
                    row_no: 0,
                    message: format!("{whereami}: unsupported YAML tag {}", tagged.tag),
                })
            }
        }
        serde_yaml_ng::Value::String(text) => text.strip_prefix("!!js ").map_or_else(
            || Ok(Value::String(text.clone())),
            |expr| {
                crate::cond::eval(expr, ctx).map_err(|reason| Error::PatchLine {
                    row_no: 0,
                    message: format!("{whereami}: {reason}"),
                })
            },
        ),
        serde_yaml_ng::Value::Null => Ok(Value::Null),
        serde_yaml_ng::Value::Bool(flag) => Ok(Value::Bool(*flag)),
        serde_yaml_ng::Value::Number(number) => {
            serde_json::to_value(number).map_err(|e| Error::PatchLine {
                row_no: 0,
                message: format!("{whereami}: unsupported number: {e}"),
            })
        }
        serde_yaml_ng::Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(convert_config(item, ctx, whereami)?);
            }
            Ok(Value::Array(out))
        }
        serde_yaml_ng::Value::Mapping(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                let key_text = match key {
                    serde_yaml_ng::Value::String(text) => text.clone(),
                    other => {
                        return Err(Error::PatchLine {
                            row_no: 0,
                            message: format!("{whereami}: non-string mapping key {other:?}"),
                        });
                    }
                };
                out.insert(key_text, convert_config(item, ctx, whereami)?);
            }
            Ok(Value::Object(out))
        }
    }
}

fn err_message(error: &Error) -> String {
    match error {
        Error::PatchLine { message, .. }
        | Error::PatchParse(message)
        | Error::Io(message)
        | Error::Profile(message)
        | Error::EnvBootstrap(message) => message.clone(),
    }
}

/// Extracts one patch row from a YAML mapping (tags preserved because the
/// whole document is deserialized into `serde_yaml_ng::Value`).
fn row_from_value(
    value: &serde_yaml_ng::Value,
    ctx: &EvalContext,
    row_no: usize,
) -> std::result::Result<PatchLine, (usize, String)> {
    let fail = |message: String| (row_no, message);
    let mapping = value
        .as_mapping()
        .ok_or_else(|| fail("patch row must be a mapping".to_owned()))?;
    let mut id = None;
    let mut name = None;
    let mut config = None;
    let mut disabled = None;
    let mut insert = None;
    for (key, item) in mapping {
        let key_text = key
            .as_str()
            .ok_or_else(|| fail(format!("non-string row key {key:?}")))?
            .to_owned();
        match key_text.as_str() {
            "id" => {
                let text = item
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| fail("id must be a non-empty string".to_owned()))?;
                id = Some(text.to_owned());
            }
            "name" => {
                let text = item
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| {
                        fail("name must be a non-empty string when present".to_owned())
                    })?;
                name = Some(text.to_owned());
            }
            "disabled" => {
                let flag = item
                    .as_bool()
                    .ok_or_else(|| fail("disabled must be a boolean".to_owned()))?;
                disabled = Some(flag);
            }
            "config" => {
                let converted = convert_config(item, ctx, &format!("row {row_no} config"))
                    .map_err(|error| (row_no, err_message(&error)))?;
                if !converted.is_null() && !converted.is_object() {
                    return Err(fail("config must be an object or null".to_owned()));
                }
                config = Some(converted);
            }
            "insert" => {
                let seq = item
                    .as_sequence()
                    .ok_or_else(|| fail("insert must be a sequence of rows".to_owned()))?;
                let mut inserted = Vec::with_capacity(seq.len());
                for (idx, inner) in seq.iter().enumerate() {
                    let row = row_from_value(inner, ctx, row_no)
                        .map_err(|(_, message)| (row_no, format!("insert[{idx}]: {message}")))?;
                    if row.insert.is_some() {
                        return Err(fail(format!(
                            "insert[{idx}]: nested insert groups are not supported"
                        )));
                    }
                    if row.id.is_none() || row.name.is_none() {
                        return Err(fail(format!(
                            "insert[{idx}]: inserted rows require both id and name"
                        )));
                    }
                    inserted.push(row);
                }
                insert = Some(inserted);
            }
            other => return Err(fail(format!("unknown row key `{other}`"))),
        }
    }
    if insert.is_some() {
        if id.is_some() || name.is_some() || config.is_some() || disabled.is_some() {
            return Err(fail(
                "insert group must not carry id, name, config, or disabled".to_owned(),
            ));
        }
        return Ok(PatchLine {
            id: None,
            name: None,
            config: None,
            disabled: None,
            insert,
        });
    }
    let id = id.ok_or_else(|| fail("id must be a non-empty string".to_owned()))?;
    Ok(PatchLine {
        id: Some(id),
        name,
        config,
        disabled,
        insert: None,
    })
}

/// Parses a `fr.patch.yml` document (a YAML sequence of rows), evaluating
/// restricted-subset expressions with `ctx`.
///
/// # Errors
///
/// Returns [`Error::PatchParse`] when the document is not a valid YAML
/// sequence of rows and [`Error::PatchLine`] when a row violates the row
/// schema or an expression falls outside the restricted subset (the whole
/// document is rejected — fail-closed, matching dsh's throw on user patch
/// errors).
///
/// # Implementation note
///
/// libyaml expands the `!!js` secondary-handle tag to
/// `tag:yaml.org,2002:js`, which `serde_yaml_ng` drops for quoted scalars;
/// the single-bang form survives as a `Value::Tagged`. Value positions are
/// normalized to the single-bang form line-anchored before parsing so quoted
/// content is untouched.
///
/// # Panics
///
/// Never at runtime: the internal regex patterns are constant and compiled
/// through `expect` with a statically valid pattern.
pub fn parse_patch_lines(yaml: &str, ctx: &EvalContext) -> Result<Vec<PatchLine>> {
    // libyaml expands the `!!js` secondary-handle tag to
    // `tag:yaml.org,2002:js`, which serde_yaml_ng drops for quoted scalars;
    // the single-bang form survives as a `Value::Tagged`. Normalize value
    // positions `!!js` -> `!js` line-anchored so quoted content is untouched.
    let normalized = normalize_js_tags(yaml);
    ensure_no_flow_style_js(&normalized)?;
    let doc: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&normalized).map_err(|e| Error::PatchParse(e.to_string()))?;
    let seq = doc.as_sequence().ok_or_else(|| {
        Error::PatchParse("patch document must be a YAML sequence of rows".to_owned())
    })?;
    let mut lines = Vec::with_capacity(seq.len());
    for (idx, item) in seq.iter().enumerate() {
        lines.push(
            row_from_value(item, ctx, row_no_of(idx))
                .map_err(|(row_no, message)| Error::PatchLine { row_no, message })?,
        );
    }
    Ok(lines)
}

/// Matches `!!js` left over after normalization: value positions not covered
/// by [`normalize_js_tags`] (flow style, exotic keys).
static REMAINING_JS_TAG: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// Rewrites `!!js` tag occurrences at YAML value positions (after `key:` or
/// after a list dash) to the single-bang form; quoted content never matches.
#[must_use]
pub(crate) fn normalize_js_tags(yaml: &str) -> String {
    let value_position =
        Regex::new(r"(?m)^([ \t]*(?:-[ \t]+)?[A-Za-z_][A-Za-z0-9_-]*:[ \t]+|^[ \t]*-[ \t]+)!!js")
            .expect("valid regex");
    value_position.replace_all(yaml, "${1}!js").into_owned()
}

/// Rejects `!!js` tags left over after normalization: value positions not
/// covered by [`normalize_js_tags`] (flow style, exotic keys). The fr parse
/// dialect cannot preserve a tag there, so failing closed beats silently
/// dropping it. Shared by the boot-grade and the dump raw parses.
///
/// # Errors
///
/// [`Error::PatchParse`] when a flow-style `!!js` value position remains.
pub(crate) fn ensure_no_flow_style_js(normalized: &str) -> Result<()> {
    if REMAINING_JS_TAG
        .get_or_init(|| {
            // Non-anchored: any remaining `key: !!js` or `- !!js` value slot.
            Regex::new(r"(:[ \t]+|[ \t]-[ \t]+)!!js").expect("valid regex")
        })
        .is_match(normalized)
    {
        return Err(Error::PatchParse(
            "`!!js` tag outside a recognized value position (flow style is not supported); \
             write `key: !!js \"expr\"` on its own line"
                .to_owned(),
        ));
    }
    Ok(())
}

/// A fully resolved assembly row after composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatRow {
    /// Composition key.
    pub id: String,
    /// Plugin name (resolved to an artifact at compile time).
    pub name: String,
    /// Plugin configuration (object or null).
    pub config: Value,
    /// Enablement.
    pub disabled: bool,
}

/// Result of composing patch layers: effective rows plus warn-skip records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeOutcome {
    /// Effective rows in first-insertion order.
    pub rows: Vec<FlatRow>,
    /// Rows skipped with a warning (unmatched id, name mismatch, missing id).
    pub warnings: Vec<String>,
}

/// Composes patch layers in application order (bottom layer first).
///
/// Overrides address rows by `id` (and optionally `name`); unmatched or
/// mismatched overrides warn and skip without failing. `insert` groups append
/// rows to the root; later layers can override inserted rows.
#[must_use]
pub fn compose(layers: &[Vec<PatchLine>]) -> ComposeOutcome {
    let mut rows: Vec<FlatRow> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut warnings: Vec<String> = Vec::new();

    let build_map =
        |rows: &[FlatRow], index: &mut std::collections::HashMap<String, usize>, from: usize| {
            for (offset, row) in rows.iter().enumerate().skip(from) {
                index.insert(row.id.clone(), offset);
            }
        };

    for layer in layers {
        for line in layer {
            if let Some(inserted) = &line.insert {
                let from = rows.len();
                for inner in inserted {
                    // Parse guarantees id and name on inserted rows.
                    let id = inner.id.clone().unwrap_or_default();
                    rows.push(FlatRow {
                        id,
                        name: inner.name.clone().unwrap_or_default(),
                        config: inner.config.clone().unwrap_or(Value::Null),
                        disabled: inner.disabled.unwrap_or(false),
                    });
                }
                build_map(&rows, &mut index, from);
                continue;
            }
            let Some(id) = &line.id else {
                warnings.push("patch row without id skipped".to_owned());
                continue;
            };
            let Some(&position) = index.get(id) else {
                warnings.push(format!("patch: entry {id} not found, skipped"));
                continue;
            };
            if let Some(name) = &line.name
                && rows[position].name != *name
            {
                warnings.push(format!(
                    "patch: name mismatch for {id} (expected {}, got {name}), skipped",
                    rows[position].name
                ));
                continue;
            }
            if line.config.is_some() {
                rows[position].config = line.config.clone().unwrap_or(Value::Null);
            }
            if line.disabled.is_some() {
                rows[position].disabled = line.disabled.unwrap_or(false);
            }
        }
    }
    ComposeOutcome { rows, warnings }
}

/// One row that failed artifact resolution or trust-boundary validation.
///
/// Failed rows are excluded from the compiled document (fail-closed) without
/// blocking the remaining slots, mirroring the loader `Report::failures`
/// semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileFailure {
    /// Id of the failing patch row.
    pub id: String,
    /// Plugin name of the failing row.
    pub name: String,
    /// Human-readable failure reason.
    pub reason: String,
}

/// Result of compiling rows: the loader document plus per-row failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileOutcome {
    /// Loader JSON document containing only successfully resolved rows.
    pub doc: String,
    /// Rows excluded from the document, with reasons.
    pub failures: Vec<CompileFailure>,
}

/// Compiles composed rows into a dougong loader JSON document.
///
/// Disabled rows are excluded. Each row's plugin name is resolved to a
/// dylib/wasm artifact via `resolver` (PRD 6.2 rule; the `builtin` source
/// kind is never emitted) and validated against the trust boundary via
/// `is_first_party`. When `schema_of` declares a config schema for the row's
/// plugin (PRD 6.3), the row's config is validated against it; rows whose
/// plugin has no declaration are skipped (the dsh `absent` semantics,
/// config-schema/types.ts:42). Failed rows land in
/// [`CompileOutcome::failures`].
///
/// # Errors
///
/// Returns [`Error::PatchParse`] if serialization fails, which cannot happen
/// for values built from validated rows but is kept for totality.
pub fn compile_to_loader_doc<'s>(
    lines: &[FlatRow],
    resolver: &PathResolver,
    is_first_party: &dyn Fn(&str) -> bool,
    schema_of: &dyn Fn(&str) -> Option<&'s fr_schema::Schema>,
) -> Result<CompileOutcome> {
    let mut plugins = Vec::new();
    let mut failures = Vec::new();
    for line in lines.iter().filter(|line| !line.disabled) {
        let spec = match resolver.resolve(&line.name) {
            Ok(spec) => spec,
            Err(reason) => {
                failures.push(CompileFailure {
                    id: line.id.clone(),
                    name: line.name.clone(),
                    reason,
                });
                continue;
            }
        };
        if let Some(reason) = crate::source::check_trust_boundary(&line.name, &spec, is_first_party)
        {
            failures.push(CompileFailure {
                id: line.id.clone(),
                name: line.name.clone(),
                reason,
            });
            continue;
        }
        if let Some(schema) = schema_of(&line.name) {
            let violations = fr_schema::validate(schema, &line.config);
            if !violations.is_empty() {
                failures.push(CompileFailure {
                    id: line.id.clone(),
                    name: line.name.clone(),
                    reason: format_config_violations(&violations),
                });
                continue;
            }
        }
        plugins.push(serde_json::json!({
            "name": line.id,
            "source": { "kind": spec.kind.as_str(), "path": spec.path.display().to_string() },
            "config": line.config,
            "isolate": Value::Null,
        }));
    }
    let doc = serde_json::json!({ "plugins": plugins });
    let doc = serde_json::to_string(&doc).map_err(|e| Error::PatchParse(e.to_string()))?;
    Ok(CompileOutcome { doc, failures })
}

/// Renders schema violations as the compile failure reason (PRD 6.3): each
/// violation becomes `path (message)` with the root shown as `<root>`.
fn format_config_violations(violations: &[fr_schema::Violation]) -> String {
    let mut reason = String::from("config schema violations:");
    for violation in violations {
        let path = if violation.path.is_empty() {
            "<root>"
        } else {
            &violation.path
        };
        // `String`'s `fmt::Write` impl is infallible; the Result is always Ok.
        let _ = write!(reason, " {path} ({});", violation.message);
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceKind;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    const SAMPLE: &str = "\
- insert:
    - id: llm-primary
      name: fr-llm-deepseek
      config:
        model: deepseek-flash
    - id: session-store
      name: fr-session-persistence
    - id: disabled-by-default
      name: fr-legacy
      disabled: true
";

    /// Creates a unique temp directory pre-populated with empty artifact
    /// files; removes the directory on drop.
    struct TempArtifacts {
        dir: PathBuf,
    }

    impl TempArtifacts {
        fn new(label: &str, files: &[&str]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("fr-config-test-{}-{label}", std::process::id()));
            fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
            for file in files {
                fs::write(dir.join(file), b"")
                    .unwrap_or_else(|e| panic!("write {file} failed: {e}"));
            }
            Self { dir }
        }

        fn resolver(&self) -> PathResolver {
            PathResolver::new(vec![self.dir.clone()])
        }
    }

    impl Drop for TempArtifacts {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn first_party_is_fr_prefix(name: &str) -> bool {
        name.starts_with("fr-")
    }

    fn neutral_ctx() -> EvalContext {
        EvalContext::neutral()
    }

    #[test]
    fn parses_wellformed_document() {
        let lines = parse_patch_lines(SAMPLE, &neutral_ctx())
            .unwrap_or_else(|e| panic!("parse failed: {e}"));
        assert_eq!(lines.len(), 1, "one insert group");
        let inserted = lines[0].insert.as_ref().expect("insert group");
        assert_eq!(inserted.len(), 3);
        assert_eq!(inserted[0].id.as_deref(), Some("llm-primary"));
        assert_eq!(inserted[0].name.as_deref(), Some("fr-llm-deepseek"));
        assert_eq!(inserted[0].config, Some(json!({"model": "deepseek-flash"})));
        assert_eq!(inserted[0].disabled, None);
        assert_eq!(inserted[1].config, None);
        assert_eq!(inserted[2].disabled, Some(true));
    }

    #[test]
    fn toggle_only_override_keeps_other_keys() {
        let base = parse_patch_lines(SAMPLE, &neutral_ctx())
            .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let overlay = parse_patch_lines("- id: llm-primary\n  disabled: true\n", &neutral_ctx())
            .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let outcome = compose(&[base, overlay]);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        let row = outcome
            .rows
            .iter()
            .find(|row| row.id == "llm-primary")
            .unwrap_or_else(|| panic!("row missing"));
        assert!(row.disabled);
        // Config and name survive a disabled-only override.
        assert_eq!(row.config, json!({"model": "deepseek-flash"}));
        assert_eq!(row.name, "fr-llm-deepseek");
    }

    #[test]
    fn config_override_replaces_wholesale() {
        let base = parse_patch_lines(SAMPLE, &neutral_ctx())
            .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let overlay = parse_patch_lines(
            "- id: llm-primary\n  config:\n    model: deepseek-reasoner\n",
            &neutral_ctx(),
        )
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let outcome = compose(&[base, overlay]);
        let row = outcome
            .rows
            .iter()
            .find(|row| row.id == "llm-primary")
            .unwrap_or_else(|| panic!("row missing"));
        assert_eq!(row.config, json!({"model": "deepseek-reasoner"}));
    }

    #[test]
    fn unmatched_and_mismatched_overrides_warn_and_skip() {
        let base = parse_patch_lines(SAMPLE, &neutral_ctx())
            .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let overlay = parse_patch_lines(
            "- id: nonexistent\n  disabled: true\n- id: llm-primary\n  name: fr-other\n  disabled: true\n",
            &neutral_ctx(),
        )
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let outcome = compose(&[base, overlay]);
        assert_eq!(outcome.warnings.len(), 2, "{:?}", outcome.warnings);
        let row = outcome
            .rows
            .iter()
            .find(|row| row.id == "llm-primary")
            .unwrap_or_else(|| panic!("row missing"));
        assert!(!row.disabled, "mismatched override must not apply");
    }

    #[test]
    fn insert_appends_and_later_layers_can_override() {
        let base = parse_patch_lines(
            "- insert:\n    - id: headless-startup\n      name: fr-headless\n    - id: second\n      name: fr-second\n",
            &neutral_ctx(),
        )
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let overlay =
            parse_patch_lines("- id: headless-startup\n  disabled: true\n", &neutral_ctx())
                .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let outcome = compose(&[base, overlay]);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert_eq!(outcome.rows.len(), 2);
        // First-insertion order preserved.
        assert_eq!(outcome.rows[0].id, "headless-startup");
        assert_eq!(outcome.rows[1].id, "second");
        assert!(
            outcome.rows[0].disabled,
            "inserted row overridable by later layer"
        );
    }

    #[test]
    fn compiles_resolved_rows_and_excludes_disabled() {
        let temp = TempArtifacts::new(
            "compile",
            &["libfr_llm_deepseek.so", "libfr_session_persistence.so"],
        );
        let lines = parse_patch_lines(SAMPLE, &neutral_ctx())
            .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let composed = compose(&[lines]);
        let outcome = compile_to_loader_doc(
            &composed.rows,
            &temp.resolver(),
            &first_party_is_fr_prefix,
            &no_schemas,
        )
        .unwrap_or_else(|e| panic!("compile failed: {e}"));
        assert!(
            outcome.failures.is_empty(),
            "unexpected failures: {:?}",
            outcome.failures
        );
        let value: Value =
            serde_json::from_str(&outcome.doc).unwrap_or_else(|e| panic!("bad json: {e}"));
        let plugins = value["plugins"].as_array().expect("plugins array");
        assert_eq!(plugins.len(), 2, "disabled row excluded: {value}");
        assert_eq!(plugins[0]["name"], "llm-primary");
        assert_eq!(plugins[0]["source"]["kind"], "dylib");
        assert!(
            plugins[0]["source"]["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("libfr_llm_deepseek.so")),
            "resolved dylib path: {value}"
        );
        assert_eq!(plugins[0]["config"], json!({"model": "deepseek-flash"}));
    }

    #[test]
    fn compiled_document_is_valid_loader_config() {
        let temp = TempArtifacts::new(
            "loader-contract",
            &["libfr_llm_deepseek.so", "libfr_session_persistence.so"],
        );
        let lines = parse_patch_lines(SAMPLE, &neutral_ctx())
            .unwrap_or_else(|e| panic!("parse failed: {e}"));
        let composed = compose(&[lines]);
        let outcome = compile_to_loader_doc(
            &composed.rows,
            &temp.resolver(),
            &first_party_is_fr_prefix,
            &no_schemas,
        )
        .unwrap_or_else(|e| panic!("compile failed: {e}"));
        // Contract test against the real kernel loader: the compiled document
        // must be accepted by dougong-loader's parser.
        dougong_loader::Config::parse(&outcome.doc)
            .unwrap_or_else(|e| panic!("loader rejected compiled doc: {e}"));
    }

    #[test]
    fn first_party_wasm_resolution_is_rejected() {
        let temp = TempArtifacts::new("trust", &["fr_demo.wasm"]);
        let yaml = "- insert:\n    - id: demo\n      name: fr-demo\n";
        let lines =
            parse_patch_lines(yaml, &neutral_ctx()).unwrap_or_else(|e| panic!("parse failed: {e}"));
        let outcome = compile_to_loader_doc(
            &compose(&[lines]).rows,
            &temp.resolver(),
            &first_party_is_fr_prefix,
            &no_schemas,
        )
        .unwrap_or_else(|e| panic!("compile failed: {e}"));
        assert_eq!(outcome.failures.len(), 1, "{:?}", outcome.failures);
        assert_eq!(outcome.failures[0].id, "demo");
        assert!(
            outcome.failures[0]
                .reason
                .contains("trust boundary violation"),
            "{}",
            outcome.failures[0].reason
        );
    }

    #[test]
    fn literal_paths_skip_resolution() {
        let temp = TempArtifacts::new("literal", &[]);
        let yaml = "- insert:\n    - id: a\n      name: /opt/fr/lib/libfoo.so\n    - id: b\n      name: /opt/fr/lib/bar.wasm\n";
        let lines =
            parse_patch_lines(yaml, &neutral_ctx()).unwrap_or_else(|e| panic!("parse failed: {e}"));
        let outcome = compile_to_loader_doc(
            &compose(&[lines]).rows,
            &temp.resolver(),
            &first_party_is_fr_prefix,
            &no_schemas,
        )
        .unwrap_or_else(|e| panic!("compile failed: {e}"));
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        let value: Value =
            serde_json::from_str(&outcome.doc).unwrap_or_else(|e| panic!("bad json: {e}"));
        let plugins = value["plugins"].as_array().expect("plugins");
        assert_eq!(plugins[0]["source"]["kind"], "dylib");
        assert_eq!(plugins[1]["source"]["kind"], SourceKind::Wasm.as_str());
    }

    #[test]
    fn js_tagged_values_evaluate_through_subset() {
        let yaml = "- id: approval\n  name: fr-approval\n  config:\n    policy: !!js \"(env.FR_PERMISSION_MODE ?? 'workspace-write') === 'danger-full-access' ? 'never' : 'ask'\"\n";
        let lines =
            parse_patch_lines(yaml, &neutral_ctx()).unwrap_or_else(|e| panic!("parse failed: {e}"));
        assert_eq!(
            lines[0].config,
            Some(json!({"policy": "ask"})),
            "neutral ctx (no env, linux): default branch"
        );
    }

    #[test]
    fn flow_style_js_tag_is_rejected_fail_closed() {
        // Flow style is outside the recognized value positions; a dropped tag
        // would silently evaluate to the raw expression string.
        let yaml = "- id: bad\n  name: fr-bad\n  config: {p: !!js \"process.env.X\"}\n";
        let err =
            parse_patch_lines(yaml, &neutral_ctx()).expect_err("flow-style !!js must be rejected");
        assert!(
            err.to_string()
                .contains("outside a recognized value position"),
            "{err}"
        );
    }

    #[test]
    fn expression_outside_subset_rejects_document() {
        let yaml = "- id: bad\n  name: fr-bad\n  config:\n    p: !!js \"process.getBuiltinModule('node:path')\"\n";
        let err = parse_patch_lines(yaml, &neutral_ctx()).expect_err("subset escape must fail");
        assert!(matches!(err, Error::PatchLine { .. }), "{err}");
    }

    #[test]
    fn rejects_insert_group_carrying_entry_keys() {
        let yaml = "- id: a\n  insert:\n    - id: b\n      name: n\n";
        let err = parse_patch_lines(yaml, &neutral_ctx()).expect_err("insert with id must fail");
        assert!(matches!(err, Error::PatchLine { .. }), "{err}");
    }

    #[test]
    fn rejects_inserted_row_missing_name() {
        let yaml = "- insert:\n    - id: b\n";
        let err = parse_patch_lines(yaml, &neutral_ctx()).expect_err("inserted row needs id+name");
        assert!(matches!(err, Error::PatchLine { .. }), "{err}");
    }

    #[test]
    fn rejects_unknown_row_key() {
        let yaml = "- id: a\n  name: n\n  bogus: 1\n";
        let err =
            parse_patch_lines(yaml, &neutral_ctx()).expect_err("unknown key must be rejected");
        assert!(matches!(err, Error::PatchLine { row_no: 1, .. }), "{err}");
    }

    #[test]
    fn rejects_non_object_config() {
        let yaml = "- id: a\n  name: n\n  config: [1, 2]\n";
        let err =
            parse_patch_lines(yaml, &neutral_ctx()).expect_err("array config must be rejected");
        assert!(matches!(err, Error::PatchLine { row_no: 1, .. }), "{err}");
    }

    #[test]
    fn rejects_non_sequence_document() {
        let err =
            parse_patch_lines("id: a\n", &neutral_ctx()).expect_err("mapping doc must be rejected");
        assert!(matches!(err, Error::PatchParse(_)));
    }

    fn no_schemas(_: &str) -> Option<&'static fr_schema::Schema> {
        None
    }

    fn plugin_schema_fixture() -> fr_schema::Schema {
        fr_schema::Schema::Object {
            properties: vec![fr_schema::Property {
                name: "model".to_string(),
                required: true,
                schema: fr_schema::Schema::String {
                    candidates: vec!["deepseek-flash".to_string()],
                    default: None,
                    description: None,
                },
            }],
            additional_properties: false,
            description: None,
        }
    }

    #[test]
    fn schema_violation_fails_row_without_blocking() {
        let temp = TempArtifacts::new(
            "schema-fail",
            &["libfr_llm_deepseek.so", "libfr_session_persistence.so"],
        );
        let yaml = "- insert:\n    - id: llm\n      name: fr-llm-deepseek\n      config:\n        model: other\n    - id: session\n      name: fr-session-persistence\n";
        let lines =
            parse_patch_lines(yaml, &neutral_ctx()).unwrap_or_else(|e| panic!("parse failed: {e}"));
        let composed = compose(&[lines]);
        let schema = plugin_schema_fixture();
        let schema_by_name = |name: &str| match name {
            "fr-llm-deepseek" => Some(&schema),
            _ => None,
        };
        let outcome = compile_to_loader_doc(
            &composed.rows,
            &temp.resolver(),
            &first_party_is_fr_prefix,
            &schema_by_name,
        )
        .unwrap_or_else(|e| panic!("compile failed: {e}"));
        assert_eq!(outcome.failures.len(), 1, "{:?}", outcome.failures);
        assert_eq!(outcome.failures[0].id, "llm");
        assert!(
            outcome.failures[0]
                .reason
                .contains("config schema violations"),
            "{}",
            outcome.failures[0].reason
        );
        assert!(
            outcome.failures[0].reason.contains("model"),
            "{}",
            outcome.failures[0].reason
        );
        // The failing row is excluded fail-closed; the schema-less row loads
        // normally (absent semantics, PRD 6.3).
        let value: Value =
            serde_json::from_str(&outcome.doc).unwrap_or_else(|e| panic!("bad json: {e}"));
        let plugins = value["plugins"].as_array().expect("plugins array");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["name"], "session");
    }

    #[test]
    fn absent_schema_skips_validation() {
        let temp = TempArtifacts::new("schema-absent", &["libfr_llm_deepseek.so"]);
        let yaml = "- insert:\n    - id: llm\n      name: fr-llm-deepseek\n      config:\n        model: anything-goes\n";
        let lines =
            parse_patch_lines(yaml, &neutral_ctx()).unwrap_or_else(|e| panic!("parse failed: {e}"));
        let composed = compose(&[lines]);
        let outcome = compile_to_loader_doc(
            &composed.rows,
            &temp.resolver(),
            &first_party_is_fr_prefix,
            &no_schemas,
        )
        .unwrap_or_else(|e| panic!("compile failed: {e}"));
        assert_eq!(outcome.failures.len(), 0, "{:?}", outcome.failures);
        assert!(outcome.doc.contains("anything-goes"), "{}", outcome.doc);
    }
}
