//! Offline config-schema dump (PRD 6.3; 2026-10-22 Q18/Q26B rulings).
//!
//! `dump_config_schema` inspects a composed entry list without mounting
//! plugins or evaluating expressions: rows are validated shape-only, each
//! name is looked up through the caller's schema resolver, and the result
//! is assembled into one JSON Schema 2020-12 document with the `x-fr`
//! annotation block (dsh `x-cordis`). The `$defs` skeleton is the 26B
//! public face (PRD 19.2 item 13): loaderExpression, entryMetadata,
//! entryList, patchList, unknownConfig, entry, patch, plus one `configN`
//! definition per projected plugin declaration. dsh's include/group
//! carrier definitions and builtin name constants are deliberately absent —
//! fr assemblies carry no builtin forms (PRD 4), so the document must not
//! claim to accept rows fr rejects.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;
use serde_yaml_ng::Value as RowValue;

use crate::Schema;
use crate::to_json_schema;

/// Status of one discovered entry (dsh `ConfigSchemaEntry.status`, five states).
///
/// `Partial` and `Unsupported` are structurally unreachable while fr-schema
/// descriptors are fail-closed at construction and [`to_json_schema`] is
/// total; the vocabulary is kept because the `x-fr` entry surface pins all
/// five states (PRD 19.2 item 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryStatus {
    /// A declaration was found and projected to a `configN` definition.
    Schema,
    /// Projected with limitations (unreachable in fr-schema; see type docs).
    Partial,
    /// No declaration found for the row's plugin name (dsh absent, Q9A).
    Absent,
    /// A declaration exists but is outside the projectable subset
    /// (unreachable in fr-schema; see type docs).
    Unsupported,
    /// The row is malformed or its projection failed.
    Error,
}

impl EntryStatus {
    /// The wire spelling used inside the `x-fr` block.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Partial => "partial",
            Self::Absent => "absent",
            Self::Unsupported => "unsupported",
            Self::Error => "error",
        }
    }
}

/// Native tree carrier of an entry. Retained for `x-fr` entry shape
/// fidelity; fr data never produces it because fr assemblies carry no
/// builtin group/include forms (PRD 19.2 item 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKind {
    /// `cordis:group`-shaped carrier (never produced by fr data).
    Group,
    /// `cordis:include`-shaped carrier (never produced by fr data).
    Include,
}

impl TreeKind {
    /// The wire spelling used inside the `x-fr` block.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Include => "include",
        }
    }
}

/// Severity of a collection or projection diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    /// Informational shortfall; does not by itself break completeness.
    Warning,
    /// Collection or projection failure; forces `complete` false.
    Error,
}

impl DiagnosticLevel {
    /// The wire spelling used inside the `x-fr` block.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// One discovered entry inside the `x-fr` block (dsh `ConfigSchemaEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaEntry {
    /// Structural position in the composed tree: `/0`, `/1`, ...
    pub path: String,
    /// The row's literal `id`, when it is a literal string.
    pub id: Option<String>,
    /// The row's literal plugin name, when it is a literal string.
    pub name: Option<String>,
    /// Discovery or projection status.
    pub status: EntryStatus,
    /// JSON pointer to the `$defs` entry describing this row's config.
    pub config_ref: Option<String>,
    /// Native tree carrier; always `None` in fr data (see [`TreeKind`]).
    pub tree: Option<TreeKind>,
}

/// One collection or projection diagnostic (dsh `ConfigSchemaDiagnostic`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDiagnostic {
    /// Severity.
    pub level: DiagnosticLevel,
    /// Entry path the diagnostic belongs to, when positioned.
    pub path: Option<String>,
    /// Human-readable description.
    pub message: String,
}

/// The schema-dump result: one complete JSON Schema document plus the
/// `x-fr` facts mirrored out for callers (dsh reads them back off
/// `dump['x-cordis']`).
#[derive(Debug, Clone)]
pub struct SchemaDump {
    /// The full document as printed to stdout, `x-fr` block included.
    pub document: Value,
    /// False when any error diagnostic, `partial`/`unsupported`/`error`
    /// entry, or ambiguous plugin-name projection exists (dsh completeness).
    pub complete: bool,
    /// Diagnostics in printed order (sorted by entry position, stable).
    pub diagnostics: Vec<SchemaDiagnostic>,
}

/// Fixed `$defs` pointer for rows without a projected declaration (dsh
/// `unknownConfig`).
const UNKNOWN_CONFIG: &str = "#/$defs/unknownConfig";
/// Fixed pointer to the patch-list definition advertised by `x-fr`.
const PATCH_SCHEMA: &str = "#/$defs/patchList";

/// Builds the `dump-config-schema` document from composed rows (dsh
/// `collectConfigSchemas` + `buildConfigSchemaDocument`, 26B public face).
///
/// Rows are validated shape-only with dsh's exact messages; a malformed row
/// becomes a positioned error diagnostic and an `error` entry without
/// losing its siblings. Every validated name goes through `schema_of`: a
/// hit projects to a shared `configN` definition and `schema` status, a
/// miss marks the row `absent` with the `unknownConfig` reference (Q9A). Initial
/// diagnostics (composition warnings, skipped bundles) are copied in ahead
/// of walk diagnostics and everything is sorted by entry position.
#[must_use]
pub fn dump_config_schema<'s>(
    profile: &str,
    rows: &[RowValue],
    schema_of: &dyn Fn(&str) -> Option<&'s Schema>,
    initial_diagnostics: Vec<SchemaDiagnostic>,
) -> SchemaDump {
    let mut diagnostics = initial_diagnostics;
    let (mut collected, valid) = walk_rows(rows, schema_of, &mut diagnostics);
    // Patch targets: last id wins across the composed tree; an invalid row
    // carrying an id clears it (dsh `indexTargets` with byOptions misses).
    let mut targets: BTreeMap<String, usize> = BTreeMap::new();
    index_targets(rows, &valid, true, &mut targets);
    let mut definitions = base_definitions();
    project_definitions(&mut collected, &mut definitions);
    let names = name_choices(&collected);
    let entry_rules = entry_rules(&names, &mut diagnostics);
    let patch_rules = patch_rules(&targets, &collected);
    let entries: Vec<SchemaEntry> = collected.into_iter().map(|slot| slot.entry).collect();
    insert_entry_and_patch(&mut definitions, entry_rules, patch_rules);
    sort_diagnostics(&mut diagnostics, &entries);
    let ambiguous = names.iter().any(|(_, choices)| choices.len() > 1);
    let complete = !diagnostics
        .iter()
        .any(|diagnostic| diagnostic.level == DiagnosticLevel::Error)
        && !entries.iter().any(|entry| {
            matches!(
                entry.status,
                EntryStatus::Partial | EntryStatus::Unsupported | EntryStatus::Error
            )
        })
        && !ambiguous;
    let document = assemble_document(profile, &entries, &diagnostics, definitions, complete);
    SchemaDump {
        document,
        complete,
        diagnostics,
    }
}

/// Walks the composed rows shape-first (dsh `collectConfigSchemas` walk):
/// every row yields an entry, a malformed row records a positioned error
/// without losing its siblings, and each validated name goes through
/// `schema_of`.
fn walk_rows<'s>(
    rows: &[RowValue],
    schema_of: &dyn Fn(&str) -> Option<&'s Schema>,
    diagnostics: &mut Vec<SchemaDiagnostic>,
) -> (Vec<Collected<'s>>, Vec<usize>) {
    let mut collected: Vec<Collected<'_>> = Vec::new();
    let mut valid: Vec<usize> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let path = format!("/{index}");
        let mut entry = SchemaEntry {
            path,
            id: literal_string(row, "id"),
            name: literal_string(row, "name"),
            status: EntryStatus::Error,
            config_ref: None,
            tree: None,
        };
        let mut schema = None;
        match validate_entry(row) {
            None => {
                entry.status = EntryStatus::Absent;
                if let Some(hit) = entry.name.as_deref().and_then(schema_of) {
                    schema = Some(hit);
                    entry.status = EntryStatus::Schema;
                }
                valid.push(index);
            }
            Some(message) => {
                diagnostics.push(SchemaDiagnostic {
                    level: DiagnosticLevel::Error,
                    path: Some(entry.path.clone()),
                    message,
                });
            }
        }
        collected.push(Collected { entry, schema });
    }
    (collected, valid)
}

/// The fixed `$defs` skeleton shared by every document (dsh document.ts
/// definitions minus the cut include carrier face).
fn base_definitions() -> BTreeMap<String, Value> {
    let mut definitions: BTreeMap<String, Value> = BTreeMap::new();
    definitions.insert("loaderExpression".into(), loader_expression());
    definitions.insert("entryMetadata".into(), entry_metadata());
    definitions.insert(
        "entryList".into(),
        object([
            ("type", string("array")),
            ("items", definition_ref("entry")),
        ]),
    );
    definitions.insert(
        "patchList".into(),
        object([
            ("type", string("array")),
            ("items", definition_ref("patch")),
        ]),
    );
    definitions.insert(
        "unknownConfig".into(),
        object([(
            "$comment",
            string("No projected Config schema is available; this is unknown configuration, not a prohibition on fields."),
        )]),
    );
    definitions
}

/// Projects registry hits to `configN` definitions cached per plugin name
/// and fills every entry's `configRef` (misses take [`UNKNOWN_CONFIG`]).
fn project_definitions(collected: &mut [Collected<'_>], definitions: &mut BTreeMap<String, Value>) {
    let mut projections: HashMap<String, String> = HashMap::new();
    for slot in collected.iter_mut() {
        if let Some(schema) = slot.schema {
            let name = slot.entry.name.clone().unwrap_or_default();
            let next = projections.len();
            let reference = projections
                .entry(name)
                .or_insert_with(|| {
                    let definition = format!("config{next}");
                    definitions.insert(definition.clone(), to_json_schema(schema));
                    format!("#/$defs/{definition}")
                })
                .clone();
            slot.entry.config_ref = Some(reference);
        }
        if slot.entry.config_ref.is_none() {
            slot.entry.config_ref = Some(UNKNOWN_CONFIG.into());
        }
    }
}

/// Groups each collected plugin name's config references in first-seen
/// order (dsh `names`).
fn name_choices(collected: &[Collected<'_>]) -> Vec<(String, Vec<String>)> {
    let mut names: Vec<(String, Vec<String>)> = Vec::new();
    for slot in collected {
        let Some(name) = slot.entry.name.clone() else {
            continue;
        };
        let reference = slot
            .entry
            .config_ref
            .clone()
            .unwrap_or_else(|| UNKNOWN_CONFIG.into());
        match names.iter_mut().find(|(existing, _)| *existing == name) {
            Some((_, choices)) => {
                if !choices.contains(&reference) {
                    choices.push(reference);
                }
            }
            None => names.push((name, vec![reference])),
        }
    }
    names
}

/// Builds the name-conditional entry rules and records the ambiguity
/// warnings (dsh entry rules in `buildConfigSchemaDocument`).
fn entry_rules(
    names: &[(String, Vec<String>)],
    diagnostics: &mut Vec<SchemaDiagnostic>,
) -> Vec<Value> {
    let mut rules: Vec<Value> = Vec::new();
    for (name, choices) in names {
        let branches: Vec<Value> = choices
            .iter()
            .map(|reference| definition_ref(reference.trim_start_matches("#/$defs/")))
            .collect();
        rules.push(object([
            (
                "if",
                object([
                    (
                        "properties",
                        object([("name", object([("const", string(name))]))]),
                    ),
                    ("required", array(vec![string("name")])),
                ]),
            ),
            (
                "then",
                object([(
                    "properties",
                    object([("config", object([("anyOf", array(branches))]))]),
                )]),
            ),
        ]));
        if choices.len() > 1 {
            diagnostics.push(SchemaDiagnostic {
                level: DiagnosticLevel::Warning,
                path: None,
                message: format!(
                    "Plugin {} has multiple collected schemas; entry validation accepts their union because module resolution depends on the owning tree.",
                    json_name(name)
                ),
            });
        }
    }
    rules
}

/// Builds the id-conditional patch rules binding overrides to their
/// target's projection (dsh patch rules).
fn patch_rules(targets: &BTreeMap<String, usize>, collected: &[Collected<'_>]) -> Vec<Value> {
    let mut rules: Vec<Value> = Vec::new();
    for (id, index) in targets {
        let Some(target) = collected.get(*index) else {
            continue;
        };
        let Some(target_name) = target.entry.name.clone() else {
            continue;
        };
        let config_ref = target
            .entry
            .config_ref
            .clone()
            .unwrap_or_else(|| UNKNOWN_CONFIG.into());
        rules.push(patch_rule(id, &target_name, &config_ref));
    }
    rules
}

/// Inserts the composed `entry` and `patch` definitions over the skeleton.
fn insert_entry_and_patch(
    definitions: &mut BTreeMap<String, Value>,
    entry_rules: Vec<Value>,
    patch_rules: Vec<Value>,
) {
    let entry_all_of = {
        let mut rules = vec![definition_ref("entryMetadata")];
        rules.extend(entry_rules);
        rules
    };
    definitions.insert(
        "entry".into(),
        object([
            ("type", string("object")),
            ("required", array(vec![string("name")])),
            ("allOf", array(entry_all_of)),
            (
                "description",
                string(
                    "dougong loader entry. Unknown metadata and plugin names are accepted; plugin-specific validation is available only for the names collected in this profile. Distinct resolutions of one name use a union of their schemas.",
                ),
            ),
        ]),
    );
    let mut patch_all_of = vec![definition_ref("entryMetadata")];
    patch_all_of.extend(patch_rules);
    let mut patch = patch_structure();
    if let Value::Object(map) = &mut patch {
        map.insert("allOf".into(), Value::Array(patch_all_of));
    }
    definitions.insert("patch".into(), patch);
}

/// Sorts diagnostics by entry position (path-less first, stable — dsh's
/// final ordering pass).
fn sort_diagnostics(diagnostics: &mut [SchemaDiagnostic], entries: &[SchemaEntry]) {
    let positions: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.path.as_str(), index))
        .collect();
    diagnostics.sort_by(|left, right| {
        let key = |diagnostic: &SchemaDiagnostic| {
            // Path-less diagnostics take key 0; positioned ones index + 1,
            // so the stable sort prints them first (dsh maps misses to -1).
            diagnostic
                .path
                .as_deref()
                .and_then(|path| positions.get(path))
                .map_or(0, |index| index + 1)
        };
        key(left).cmp(&key(right))
    });
}

/// Assembles the final document: fixed annotations, `$defs`, and the
/// `x-fr` block (dsh `buildConfigSchemaDocument` return).
fn assemble_document(
    profile: &str,
    entries: &[SchemaEntry],
    diagnostics: &[SchemaDiagnostic],
    definitions: BTreeMap<String, Value>,
    complete: bool,
) -> Value {
    let x_fr = object([
        ("profile", string(profile)),
        ("complete", Value::Bool(complete)),
        (
            "entries",
            Value::Array(entries.iter().map(entry_value).collect()),
        ),
        (
            "diagnostics",
            Value::Array(diagnostics.iter().map(diagnostic_value).collect()),
        ),
        ("patchSchema", string(PATCH_SCHEMA)),
    ]);
    object([
        (
            "$schema",
            string("https://json-schema.org/draft/2020-12/schema"),
        ),
        (
            "title",
            string(&format!("Dougong configuration for profile {profile}")),
        ),
        (
            "description",
            string(
                "Describes the parsed entry-list YAML printed by --dump-config. Use $defs.patchList for a profile/home/CLI overlay. Parse !!js with the Dougong entry-list YAML dialect. Expression results, service dependencies, plugin startup checks, and sequence-dependent patch targets still require runtime validation.",
            ),
        ),
        (
            "$comment",
            string(
                "Bundle, profile, home, and CLI layers apply in that order. A patch config replaces the whole config. JSON Schema defaults are annotations; fr-schema validates a fallback on null/omission unless required rejects it first. fr-schema unions select the first successful branch. Root patch id constraints describe the current index, not ids introduced or changed by earlier patches. Relative plugin insertions require their source patch directory; this document does not infer future overlay locations.",
            ),
        ),
        ("type", string("array")),
        ("items", definition_ref("entry")),
        ("$defs", Value::Object(definitions.into_iter().collect())),
        ("x-fr", x_fr),
    ])
}

/// One walked row: its output shape plus the resolution result used while
/// building the document.
struct Collected<'s> {
    /// Output shape inside the `x-fr` block.
    entry: SchemaEntry,
    /// Registry hit for the row's name, projected to a `configN` later.
    schema: Option<&'s Schema>,
}

/// Validates one row shape-first with dsh's exact messages; `Ok` marks the
/// row collectable. dsh's metadata loop also re-checks `name`, but the
/// entry check already guarantees a literal string there.
fn validate_entry(row: &RowValue) -> Option<String> {
    let RowValue::Mapping(_) = row else {
        return Some("each entry must be a mapping with a literal plugin name".into());
    };
    let RowValue::String(_) = row.get("name").unwrap_or(&RowValue::Null) else {
        return Some("each entry must be a mapping with a literal plugin name".into());
    };
    for key in ["id", "name"] {
        if row
            .get(key)
            .is_some_and(|value| !matches!(value, RowValue::String(_)))
        {
            return Some(format!("{key} must be a literal string"));
        }
    }
    if row
        .get("group")
        .is_some_and(|value| !matches!(value, RowValue::Bool(_) | RowValue::Null))
    {
        return Some("group must be a literal boolean or null".into());
    }
    None
}

/// Indexes patch targets over the composed tree (dsh `indexTargets`): a
/// valid collected row with a nonempty literal `id` claims the target, any
/// other row carrying the same id clears it. Descends into truthy-group
/// config arrays where no row can be a collected entry (fr walks no carrier
/// subtrees), preserving dsh's clear-on-miss semantics.
fn index_targets(
    rows: &[RowValue],
    valid: &[usize],
    collectable: bool,
    targets: &mut BTreeMap<String, usize>,
) {
    for (index, row) in rows.iter().enumerate() {
        let RowValue::Mapping(_) = row else {
            continue;
        };
        if let Some(RowValue::String(id)) = row.get("id")
            && !id.is_empty()
        {
            if collectable && valid.contains(&index) {
                targets.insert(id.clone(), index);
            } else {
                targets.remove(id);
            }
        }
        if js_truthy(row.get("group").unwrap_or(&RowValue::Null))
            && let Some(RowValue::Sequence(children)) = row.get("config")
        {
            index_targets(children, valid, false, targets);
        }
    }
}

/// JS truthiness over the YAML value domain, matching dsh's coercion in
/// `indexTargets` (`row.group && ...`).
fn js_truthy(value: &RowValue) -> bool {
    match value {
        RowValue::Bool(flag) => *flag,
        RowValue::String(text) => !text.is_empty(),
        RowValue::Number(number) => number.as_f64().is_some_and(|f| f != 0.0),
        RowValue::Mapping(_) | RowValue::Sequence(_) | RowValue::Tagged(_) => true,
        RowValue::Null => false,
    }
}

/// The row's field value when it is a literal string.
fn literal_string(row: &RowValue, key: &str) -> Option<String> {
    match row.get(key) {
        Some(RowValue::String(text)) => Some(text.clone()),
        _ => None,
    }
}

/// JSON quoting for a plugin name inside a diagnostic message (dsh
/// `JSON.stringify`).
fn json_name(name: &str) -> String {
    Value::String(name.to_owned()).to_string()
}

/// Builds a JSON object preserving the given pair order.
fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}

/// Builds a JSON array.
const fn array(items: Vec<Value>) -> Value {
    Value::Array(items)
}

/// Builds a JSON string.
fn string(text: &str) -> Value {
    Value::String(text.to_owned())
}

/// A `$ref` into the document's `$defs`.
fn definition_ref(name: &str) -> Value {
    object([("$ref", string(&format!("#/$defs/{name}")))])
}

/// The inert `!!js` marker definition (dsh `LOADER_EXPRESSION_SCHEMA`,
/// projector.ts:8-12, brand-mapped).
fn loader_expression() -> Value {
    object([
        ("type", string("object")),
        (
            "properties",
            object([("__jsExpr", object([("type", string("string"))]))]),
        ),
        ("required", array(vec![string("__jsExpr")])),
        (
            "description",
            string(
                "Inert representation of a YAML !!js scalar from the Dougong entry-list parser. Its result is evaluated and validated only at runtime. Extra marker-object fields are ignored by interpolation.",
            ),
        ),
    ])
}

/// The shared entry-metadata properties (dsh `metadata()`, brand-mapped).
fn entry_metadata() -> Value {
    let properties = object([
        (
            "id",
            object([
                ("type", string("string")),
                (
                    "description",
                    string(
                        "Entry id. The dougong loader generates an id when an entry omits it; patches use the configured id.",
                    ),
                ),
            ]),
        ),
        (
            "name",
            object([
                ("type", string("string")),
                (
                    "description",
                    string(
                        "Plugin module specifier. Inserted relative plugin paths are anchored beside their patch file.",
                    ),
                ),
            ]),
        ),
        ("config", object([])),
        (
            "group",
            object([
                ("type", array(vec![string("boolean"), string("null")])),
                (
                    "description",
                    string(
                        "Allows patch indexing and insertion into an entry-list config; does not select the plugin implementation.",
                    ),
                ),
            ]),
        ),
        (
            "disabled",
            object([
                (
                    "anyOf",
                    array(vec![
                        object([("type", array(vec![string("boolean"), string("null")]))]),
                        definition_ref("loaderExpression"),
                    ]),
                ),
                (
                    "description",
                    string(
                        "Boolean or !!js expression. The dougong loader coerces other truthy values as disabled; this schema rejects them.",
                    ),
                ),
            ]),
        ),
        (
            "inject",
            object([(
                "anyOf",
                array(vec![
                    object([
                        ("type", string("array")),
                        ("items", object([("type", string("string"))])),
                    ]),
                    object([("type", string("object"))]),
                    object([("type", string("null"))]),
                ]),
            )]),
        ),
        (
            "intercept",
            object([("type", array(vec![string("object"), string("null")]))]),
        ),
        (
            "isolate",
            object([
                ("type", array(vec![string("object"), string("null")])),
                (
                    "additionalProperties",
                    object([(
                        "anyOf",
                        array(vec![
                            object([("const", Value::Bool(true))]),
                            object([("type", string("string"))]),
                        ]),
                    )]),
                ),
            ]),
        ),
    ]);
    object([("type", string("object")), ("properties", properties)])
}

/// The patch structure shared by `$defs/patch` and (in dsh) the cut include
/// patch (dsh `patchStructure`).
fn patch_structure() -> Value {
    object([
        ("type", string("object")),
        ("allOf", array(vec![definition_ref("entryMetadata")])),
        (
            "properties",
            object([("insert", definition_ref("entryList"))]),
        ),
        (
            "description",
            string(
                "An insert appends entries, optionally inside the group identified by id. Other patches replace supplied fields; config is replaced wholesale, not deep-merged. A truthy name asserts the existing plugin name rather than renaming it. Unknown targets and non-insert patches without a nonempty id are warned and skipped.",
            ),
        ),
    ])
}

/// One id-conditional patch rule pinning an override's config to its
/// target's projection (dsh patch rules in `buildConfigSchemaDocument`).
fn patch_rule(id: &str, target_name: &str, config_ref: &str) -> Value {
    let target_ref = config_ref.trim_start_matches("#/$defs/").to_owned();
    object([
        (
            "if",
            object([
                ("required", array(vec![string("id")])),
                (
                    "properties",
                    object([("id", object([("const", string(id))]))]),
                ),
                (
                    "allOf",
                    array(vec![
                        object([("not", object([("required", array(vec![string("insert")]))]))]),
                        object([(
                            "anyOf",
                            array(vec![
                                object([(
                                    "not",
                                    object([("required", array(vec![string("name")]))]),
                                )]),
                                object([(
                                    "properties",
                                    object([(
                                        "name",
                                        object([(
                                            "enum",
                                            array(vec![string(""), string(target_name)]),
                                        )]),
                                    )]),
                                )]),
                            ]),
                        )]),
                    ]),
                ),
            ]),
        ),
        (
            "then",
            object([(
                "properties",
                object([("config", definition_ref(&target_ref))]),
            )]),
        ),
    ])
}

/// Serializes one entry for the `x-fr` block, omitting absent optional
/// fields (dsh spreads defined fields only).
fn entry_value(entry: &SchemaEntry) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("path".into(), string(&entry.path));
    if let Some(id) = &entry.id {
        map.insert("id".into(), string(id));
    }
    if let Some(name) = &entry.name {
        map.insert("name".into(), string(name));
    }
    map.insert("status".into(), string(entry.status.as_str()));
    if let Some(config_ref) = &entry.config_ref {
        map.insert("configRef".into(), string(config_ref));
    }
    if let Some(tree) = entry.tree {
        map.insert("tree".into(), string(tree.as_str()));
    }
    Value::Object(map)
}

/// Serializes one diagnostic for the `x-fr` block.
fn diagnostic_value(diagnostic: &SchemaDiagnostic) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("level".into(), string(diagnostic.level.as_str()));
    if let Some(path) = &diagnostic.path {
        map.insert("path".into(), string(path));
    }
    map.insert("message".into(), string(&diagnostic.message));
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    //! Tests for the config-schema dump face (PRD 6.3, Q26B public skeleton).

    use super::{
        Collected, DiagnosticLevel, EntryStatus, RowValue, SchemaDiagnostic, SchemaDump,
        SchemaEntry, TreeKind, diagnostic_value, dump_config_schema, entry_value, js_truthy,
        json_name, literal_string, patch_rule,
    };
    use crate::{Schema, to_json_schema};
    use serde_json::Value;

    use super::{array, definition_ref, object, string};

    /// Parses a YAML fixture into its rows.
    fn parse_rows(text: &str) -> Vec<RowValue> {
        match serde_yaml_ng::from_str(text) {
            Ok(RowValue::Sequence(rows)) => rows,
            other => panic!("fixture must parse to a sequence: {other:?}"),
        }
    }

    /// Parses a bare scalar fixture through the row-domain parser.
    fn parse_scalar(text: &str) -> RowValue {
        serde_yaml_ng::from_str(text).unwrap_or_else(|e| panic!("fixture must parse: {e}"))
    }

    /// Dumps with an always-missing resolver (the M1 empty registry).
    fn dump(rows: &[RowValue]) -> SchemaDump {
        dump_config_schema("headless", rows, &|_| None, Vec::new())
    }

    /// Extracts the `x-fr` block from a document.
    fn x_fr(document: &Value) -> &serde_json::Map<String, Value> {
        document
            .get("x-fr")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("x-fr must be an object: {document:?}"))
    }

    /// Extracts `$defs` from a document.
    fn defs(document: &Value) -> &serde_json::Map<String, Value> {
        document
            .get("$defs")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("$defs must be an object: {document:?}"))
    }

    /// Pins the five `x-fr` keys, the 2020-12 `$schema`, the constant
    /// `patchSchema`, and the absent-entry vocabulary (Q18A/Q26B face).
    #[test]
    fn x_fr_keys_and_entry_status_vocabulary() {
        let rows = parse_rows("- name: alpha\n- id: b\n  name: beta\n");
        let dumped = dump(&rows);
        assert_eq!(
            dumped
                .document
                .get("$schema")
                .unwrap_or_else(|| panic!("$schema missing")),
            "https://json-schema.org/draft/2020-12/schema"
        );
        let x = x_fr(&dumped.document);
        let keys: Vec<&String> = x.keys().collect();
        assert_eq!(
            keys,
            [
                "profile",
                "complete",
                "entries",
                "diagnostics",
                "patchSchema"
            ]
        );
        assert_eq!(
            x.get("patchSchema")
                .unwrap_or_else(|| panic!("patchSchema missing")),
            "#/$defs/patchList"
        );
        assert_eq!(x.get("profile"), Some(&string("headless")));
        assert_eq!(x.get("complete"), Some(&Value::Bool(true)));
        let entries = x
            .get("entries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("entries missing"));
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            object([
                ("path", string("/0")),
                ("name", string("alpha")),
                ("status", string("absent")),
                ("configRef", string("#/$defs/unknownConfig")),
            ])
        );
        assert_eq!(
            entries[1],
            object([
                ("path", string("/1")),
                ("id", string("b")),
                ("name", string("beta")),
                ("status", string("absent")),
                ("configRef", string("#/$defs/unknownConfig")),
            ])
        );
        for entry in entries {
            let status = entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("status missing"));
            assert!(matches!(
                status,
                "schema" | "partial" | "absent" | "unsupported" | "error"
            ));
            assert!(
                !entry
                    .as_object()
                    .is_some_and(|object| object.contains_key("tree"))
            );
        }
        assert_eq!(dumped.diagnostics, Vec::new());
    }

    /// Pins the absent face: registry miss keeps `absent` status with the
    /// `unknownConfig` reference and no `configN` definitions (Q9A).
    #[test]
    fn absent_entries_reference_unknown_config() {
        let rows = parse_rows("- name: alpha\n");
        let dumped = dump(&rows);
        let x = x_fr(&dumped.document);
        let entries = x
            .get("entries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("entries missing"));
        assert_eq!(entries[0].get("status"), Some(&string("absent")));
        assert_eq!(
            entries[0].get("configRef"),
            Some(&string("#/$defs/unknownConfig"))
        );
        let definitions = defs(&dumped.document);
        for fixed in [
            "entry",
            "entryList",
            "entryMetadata",
            "loaderExpression",
            "patch",
            "patchList",
            "unknownConfig",
        ] {
            assert!(definitions.contains_key(fixed), "missing fixed def {fixed}");
        }
        assert_eq!(
            definitions.keys().len(),
            7,
            "no configN may exist without registry hits"
        );
    }

    /// Pins the registry-hit face: `schema` status, a shared `configN`
    /// reference cached per plugin name, and the definition equal to the same
    /// declaration's `to_json_schema` projection.
    #[test]
    fn registry_hit_projects_config_defs() {
        let schema = Schema::String {
            candidates: vec!["a".to_owned(), "b".to_owned()],
            default: None,
            description: None,
        };
        let rows = parse_rows("- name: alpha\n- name: alpha\n- name: beta\n");
        let resolve = |name: &str| (name == "alpha").then_some(&schema);
        let dumped = dump_config_schema("headless", &rows, &resolve, Vec::new());
        let x = x_fr(&dumped.document);
        let entries = x
            .get("entries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("entries missing"));
        assert_eq!(entries[0].get("status"), Some(&string("schema")));
        assert_eq!(
            entries[0].get("configRef"),
            Some(&string("#/$defs/config0"))
        );
        assert_eq!(
            entries[1].get("configRef"),
            Some(&string("#/$defs/config0"))
        );
        assert_eq!(entries[2].get("status"), Some(&string("absent")));
        let definitions = defs(&dumped.document);
        assert_eq!(
            definitions.get("config0"),
            Some(&to_json_schema(&schema)),
            "config0 must equal the declaration's projection"
        );
        assert!(dumped.complete);
    }

    /// Pins the error path: a non-mapping row yields an `error` entry with a
    /// positioned diagnostic and breaks completeness (contract: complete 为假).
    #[test]
    fn error_entry_sets_diagnostics_and_incomplete() {
        let rows = parse_rows("- just a string\n");
        let dumped = dump(&rows);
        let x = x_fr(&dumped.document);
        assert_eq!(x.get("complete"), Some(&Value::Bool(false)));
        let entries = x
            .get("entries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("entries missing"));
        assert_eq!(entries[0].get("status"), Some(&string("error")));
        let diagnostics = dumped.diagnostics.clone();
        assert_eq!(
            diagnostics,
            vec![SchemaDiagnostic {
                level: DiagnosticLevel::Error,
                path: Some("/0".to_owned()),
                message: "each entry must be a mapping with a literal plugin name".to_owned(),
            }]
        );
        let printed = x
            .get("diagnostics")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("diagnostics missing"));
        assert_eq!(
            printed,
            &diagnostics.iter().map(diagnostic_value).collect::<Vec<_>>()
        );
    }

    /// Pins sibling preservation and the metadata messages for non-literal
    /// `id`/`group` fields (dsh `validateMetadata` wording).
    #[test]
    fn malformed_metadata_preserves_siblings() {
        let rows = parse_rows("- id: 42\n  name: a\n- name: b\n- group: x\n  name: c\n");
        let dumped = dump(&rows);
        let x = x_fr(&dumped.document);
        let entries = x
            .get("entries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("entries missing"));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].get("status"), Some(&string("error")));
        assert!(
            !entries[0]
                .as_object()
                .is_none_or(|object| object.contains_key("id"))
        );
        assert_eq!(entries[1].get("status"), Some(&string("absent")));
        assert_eq!(entries[2].get("status"), Some(&string("error")));
        let messages: Vec<&str> = dumped
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert_eq!(
            messages,
            [
                "id must be a literal string",
                "group must be a literal boolean or null"
            ]
        );
        assert_eq!(dumped.diagnostics[0].path, Some("/0".to_owned()));
        assert_eq!(dumped.diagnostics[1].path, Some("/2".to_owned()));
    }

    /// Pins the id-conditional patch rules: an override on a known target id is
    /// bound to that target's config reference (dsh patch rules).
    #[test]
    fn patch_targets_bind_config_ref() {
        let rows = parse_rows("- id: t\n  name: a\n");
        let dumped = dump(&rows);
        let definitions = defs(&dumped.document);
        let patch = definitions
            .get("patch")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("patch def missing"));
        let all_of = patch
            .get("allOf")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("patch allOf missing"));
        assert_eq!(all_of.len(), 2);
        assert_eq!(all_of[1], patch_rule("t", "a", "#/$defs/unknownConfig"));

        let schema = Schema::Boolean {
            default: None,
            description: None,
        };
        let resolve = |name: &str| (name == "a").then_some(&schema);
        let dumped = dump_config_schema("headless", &rows, &resolve, Vec::new());
        let definitions = defs(&dumped.document);
        let patch = definitions
            .get("patch")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("patch def missing"));
        let all_of = patch
            .get("allOf")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("patch allOf missing"));
        assert_eq!(all_of[1], patch_rule("t", "a", "#/$defs/config0"));
    }

    /// Pins the stable diagnostics order: path-less diagnostics (skipped
    /// bundles, composition warnings) print before positioned entry errors.
    #[test]
    fn no_path_diagnostics_sort_first() {
        let rows = parse_rows("- name: a\n- oops\n");
        let initial = vec![SchemaDiagnostic {
            level: DiagnosticLevel::Warning,
            path: None,
            message: "composition warning".to_owned(),
        }];
        let dumped = dump_config_schema("headless", &rows, &|_| None, initial);
        assert_eq!(dumped.diagnostics.len(), 2);
        assert_eq!(dumped.diagnostics[0].message, "composition warning");
        assert_eq!(dumped.diagnostics[0].path, None);
        assert_eq!(dumped.diagnostics[1].path, Some("/1".to_owned()));
        assert!(!dumped.complete);
    }

    /// Pins the 26B brand mapping and the fixed skeleton definitions.
    #[test]
    fn brand_and_skeleton_pins() {
        let dumped = dump(&parse_rows("- name: alpha\n"));
        assert_eq!(
            dumped.document.get("title"),
            Some(&string("Dougong configuration for profile headless"))
        );
        let definitions = defs(&dumped.document);
        let entry = definitions
            .get("entry")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("entry def missing"));
        assert_eq!(entry.get("required"), Some(&array(vec![string("name")])));
        let all_of = entry
            .get("allOf")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("entry allOf missing"));
        assert_eq!(all_of[0], definition_ref("entryMetadata"));
        let metadata = definitions
            .get("entryMetadata")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("metadata def missing"));
        let properties = metadata
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("properties missing"));
        let disabled = properties
            .get("disabled")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("disabled missing"));
        let any_of = disabled
            .get("anyOf")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("disabled anyOf missing"));
        assert_eq!(any_of[1], definition_ref("loaderExpression"));
        let loader = definitions
            .get("loaderExpression")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("loader def missing"));
        assert!(loader.contains_key("required"));
        let description = dumped
            .document
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("description missing"));
        assert!(description.contains("Dougong entry-list YAML dialect"));
        let comment = dumped
            .document
            .get("$comment")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("$comment missing"));
        assert!(comment.contains("fr-schema validates a fallback on null/omission"));
        assert!(!comment.contains("Include-local patches"));
    }

    /// Pins the target-index clear semantics (dsh `indexTargets`): an invalid
    /// row or a truthy-group child carrying an id removes that patch target.
    #[test]
    fn invalid_row_id_clears_target() {
        let invalid = parse_rows("- id: x\n  name: a\n- id: x\n");
        let dumped = dump(&invalid);
        let definitions = defs(&dumped.document);
        let patch = definitions
            .get("patch")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("patch def missing"));
        let all_of = patch
            .get("allOf")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("patch allOf missing"));
        assert_eq!(
            all_of.len(),
            1,
            "the invalid duplicate id must clear the target"
        );

        let child = parse_rows("- id: x\n  name: a\n- group: true\n  config:\n    - id: x\n");
        let dumped = dump(&child);
        let definitions = defs(&dumped.document);
        let patch = definitions
            .get("patch")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("patch def missing"));
        let all_of = patch
            .get("allOf")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("patch allOf missing"));
        assert_eq!(
            all_of.len(),
            1,
            "the group child's id must clear the target"
        );
    }

    /// Pins JS truthiness over the YAML domain (dsh `row.group &&` coercion).
    #[test]
    fn js_truthy_matches_dsh_coercion() {
        assert!(js_truthy(&RowValue::Bool(true)));
        assert!(!js_truthy(&RowValue::Bool(false)));
        assert!(!js_truthy(&RowValue::Null));
        assert!(js_truthy(&RowValue::String("x".to_owned())));
        assert!(!js_truthy(&RowValue::String(String::new())));
        assert!(!js_truthy(&parse_scalar("0")));
        assert!(js_truthy(&parse_scalar("1")));
    }

    /// Unit pins for helpers kept private to the module.
    #[test]
    fn helper_pins() {
        assert_eq!(json_name("a"), "\"a\"");
        let row: RowValue =
            serde_yaml_ng::from_str("id: x\nname: y\n").unwrap_or_else(|e| panic!("fixture: {e}"));
        assert_eq!(literal_string(&row, "id"), Some("x".to_owned()));
        assert_eq!(literal_string(&row, "missing"), None);
        let entry = SchemaEntry {
            path: "/0".to_owned(),
            id: None,
            name: Some("a".to_owned()),
            status: EntryStatus::Schema,
            config_ref: Some("#/$defs/config0".to_owned()),
            tree: Some(TreeKind::Group),
        };
        let value = entry_value(&entry);
        assert_eq!(
            value.get("tree"),
            Some(&string("group")),
            "TreeKind stays expressible on the x-fr entry face"
        );
        let empty: &Vec<Collected<'_>> = &Vec::new();
        assert_eq!(empty.len(), 0);
        assert_eq!(EntryStatus::Partial.as_str(), "partial");
        assert_eq!(EntryStatus::Unsupported.as_str(), "unsupported");
        assert_eq!(DiagnosticLevel::Warning.as_str(), "warning");
        assert_eq!(object([("a", string("b"))]), serde_json::json!({"a": "b"}));
    }
}
