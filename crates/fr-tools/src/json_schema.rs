// Rust guideline compliant 2026-10-28
//! Enforced JSON Schema subset shared by tool outputs, generated PTC mode
//! types, subagents, and workflows (dsh `core/tools/src/json-schema.ts`,
//! verbatim semantics).
//!
//! The subset accepts any JSON root, an annotation-only schema for
//! unconstrained JSON, one scalar `type`, object
//! `properties`/`required`/boolean `additionalProperties`, array `items`,
//! type-correct scalar `enum`/`const`, and exact-one `oneOf`. Unsupported or
//! misplaced keywords reject rather than being accepted without enforcement.
//!
//! Structural absences carried over from the JavaScript source (the
//! `serde_json::Value` value-semantics judgment, slice 7 `deepFreeze`/
//! `WeakSet` precedent): realm-intrinsics checks
//! (`isPlainJsonRecord`/`isPlainJsonArray` reduce to `Value::is_object`/
//! `is_array`), cycle detection (a `Value` tree cannot alias itself), the
//! lossless-JSON tail checks of validated objects/arrays (every `Value` is
//! lossless JSON by construction), and the getter-throw containment of the
//! frame machine. Number semantics keep the JavaScript number domain:
//! numeric equality compares in `f64` and negative zero is rejected.

use std::fmt;

use serde_json::{Map, Number, Value};

/// Error code of the raw-schema boundary (dsh `JsonSchemaError`).
pub const UNSUPPORTED_SCHEMA_CODE: &str = "UNSUPPORTED_SCHEMA";

/// Single-type keywords accepted by the enforced subset, in diagnostic order.
const SCHEMA_TYPES: [&str; 7] = [
    "object", "array", "string", "number", "integer", "boolean", "null",
];

/// Constraint keywords of the subset.
const CONSTRAINT_KEYWORDS: [&str; 8] = [
    "type",
    "oneOf",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
];

/// Annotation keywords of the subset (non-validating, lossless JSON).
const ANNOTATION_KEYWORDS: [&str; 4] = ["description", "title", "default", "examples"];

/// Keywords that are invalid beside `oneOf`, in diagnostic order.
const ONE_OF_SIBLING_KEYWORDS: [&str; 6] = [
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
];

/// A raw schema fell outside the enforced subset.
///
/// `violations` lists every offending path in walk order instead of stopping
/// at the first error (dsh `JsonSchemaError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonSchemaError {
    /// Individual schema violations in walk order.
    pub violations: Vec<String>,
}

impl JsonSchemaError {
    /// Wraps the collected violation texts.
    #[must_use]
    pub const fn new(violations: Vec<String>) -> Self {
        Self { violations }
    }
}

impl fmt::Display for JsonSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported JSON schema: {}", self.violations.join("; "))
    }
}

impl std::error::Error for JsonSchemaError {}

/// Whether a value is a finite JSON number other than negative zero.
fn is_json_number(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|x| !(x == 0.0 && x.is_sign_negative()))
}

/// Whether a value is a JSON number holding an integer (JS `Number.isInteger`).
fn is_json_integer(value: &Value) -> bool {
    if !is_json_number(value) {
        return false;
    }
    value.as_i64().is_some()
        || value.as_u64().is_some()
        || value.as_f64().is_some_and(|x| x.fract() == 0.0)
}

/// Scalar equality in the JavaScript number domain: numbers compare as `f64`
/// (so `2` equals `2.0` and integer precision follows the JS lossy cast).
fn json_scalar_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => number_eq(x, y),
        _ => false,
    }
}

/// Numeric equality over the JS `f64` domain; `as_f64` covers every JSON
/// number representation.
fn number_eq(x: &Number, y: &Number) -> bool {
    match (x.as_f64(), y.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Whether a scalar is valid for one declared schema type.
fn scalar_matches(schema_type: &str, value: &Value) -> bool {
    match schema_type {
        "string" => value.is_string(),
        "number" => value.is_number() && is_json_number(value),
        "integer" => value.is_number() && is_json_integer(value),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

/// JS `JSON.stringify` rendering of one scalar (numbers use the JS integer
/// form; exponent notation beyond 1e21 is a documented corner Rust prints in
/// full decimal form).
fn js_scalar_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(x) => x.to_string(),
        Value::Number(x) => js_number_to_string(x),
        Value::String(x) => serde_json::to_string(x).unwrap_or_default(),
        Value::Array(_) | Value::Object(_) => unreachable!("scalar entries only"), // unreachable: asserted scalar lists
    }
}

/// JS `Number::toString` for `JSON.stringify`: integers (zero fraction,
/// magnitude below 1e21) print without a decimal point.
#[expect(
    clippy::float_cmp,
    reason = "JS Number parity: the integer-form probe compares the exact fixpoint"
)]
fn js_number_to_string(n: &Number) -> String {
    if let Some(x) = n.as_f64()
        && !n.is_u64()
        && !n.is_i64()
        && x == x.trunc()
        && x.abs() < 1e21
    {
        return format!("{x:.0}");
    }
    n.to_string()
}

/// JS `JSON.stringify` of a scalar array (compact separators).
fn js_scalar_array_to_string(items: &[Value]) -> String {
    let rendered: Vec<String> = items.iter().map(js_scalar_to_string).collect();
    format!("[{}]", rendered.join(","))
}

/// Root-aware diagnostic path for the empty root sentinel.
const fn diagnostic_path(path: &str) -> &str {
    if path.is_empty() { "arguments" } else { path }
}

/// Object property path without a leading dot at an implicit root.
fn property_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// The seven single-type keywords of the subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaType {
    /// Object container.
    Object,
    /// Array container.
    Array,
    /// String scalar.
    String,
    /// Number scalar.
    Number,
    /// Integer scalar.
    Integer,
    /// Boolean scalar.
    Boolean,
    /// Null scalar.
    Null,
}

impl SchemaType {
    /// Parses one asserted subset type keyword; the raw walk rejects every
    /// other spelling before validation sees the node.
    fn parse(name: &str) -> Option<Self> {
        match name {
            "object" => Some(Self::Object),
            "array" => Some(Self::Array),
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "integer" => Some(Self::Integer),
            "boolean" => Some(Self::Boolean),
            "null" => Some(Self::Null),
            _ => None,
        }
    }

    /// Reads one asserted `type` keyword from a schema record.
    fn of(value: &Value) -> Option<Self> {
        value.as_str().and_then(Self::parse)
    }
}

/// Deferred work for the stack-free raw-schema walk.
enum WalkTask<'a> {
    /// Visit one node.
    Enter { node: &'a Value, path: String },
    /// Check the `oneOf` sibling keywords after the branch walk.
    OneOfTail {
        node: &'a Map<String, Value>,
        path: String,
    },
    /// Check object-only fields after the property walk.
    ObjectTail {
        node: &'a Map<String, Value>,
        path: String,
        properties: Option<&'a Value>,
    },
}

/// Validate object-only fields after the property schemas have been visited.
fn check_object_schema_tail(
    node: &Map<String, Value>,
    path: &str,
    properties: Option<&Value>,
    violations: &mut Vec<String>,
) {
    if let Some(required) = node.get("required") {
        if let Some(names) = required
            .as_array()
            .filter(|entries| entries.iter().all(serde_json::Value::is_string))
        {
            let declared = properties.and_then(Value::as_object);
            for key in names {
                let Some(key) = key.as_str() else {
                    continue;
                };
                let known = declared.is_some_and(|map| map.contains_key(key));
                if !known {
                    violations.push(format!(
                        "{path}.required names \"{key}\" which is not in properties"
                    ));
                }
            }
        } else {
            violations.push(format!("{path}.required must be an array of strings"));
        }
    }
    if let Some(flag) = node.get("additionalProperties")
        && !flag.is_boolean()
    {
        violations.push(format!("{path}.additionalProperties must be a boolean"));
    }
}

/// Check one node's keyword vocabulary and annotation types.
fn check_node_keys(record: &Map<String, Value>, path: &str, violations: &mut Vec<String>) {
    for key in record.keys() {
        if CONSTRAINT_KEYWORDS.contains(&key.as_str()) {
            continue;
        }
        if ANNOTATION_KEYWORDS.contains(&key.as_str()) {
            // Annotation values are lossless JSON by construction.
            continue;
        }
        violations.push(format!(
            "{path}.{key} is not a supported keyword (subset: \
type/oneOf/properties/required/additionalProperties/items/enum/const + \
annotations)"
        ));
    }
    if let Some(description) = record.get("description")
        && !description.is_string()
    {
        violations.push(format!("{path}.description must be a string"));
    }
    if let Some(title) = record.get("title")
        && !title.is_string()
    {
        violations.push(format!("{path}.title must be a string"));
    }
}

/// Queue the `oneOf` sibling check and branch enters for one union node.
fn check_one_of_node<'a>(
    record: &'a Map<String, Value>,
    path: &str,
    tasks: &mut Vec<WalkTask<'a>>,
    violations: &mut Vec<String>,
) {
    tasks.push(WalkTask::OneOfTail {
        node: record,
        path: path.to_owned(),
    });
    let one_of = record.get("oneOf").unwrap_or(&Value::Null);
    if let Some(branches) = one_of.as_array()
        && branches.len() >= 2
    {
        for (index, branch) in branches.iter().enumerate().rev() {
            tasks.push(WalkTask::Enter {
                node: branch,
                path: format!("{path}.oneOf[{index}]"),
            });
        }
    } else {
        violations.push(format!(
            "{path}.oneOf must be an array of at least two schemas"
        ));
    }
}

/// Walk one typed node: misplaced keywords, then the per-type sections.
fn check_typed_node<'a>(
    record: &'a Map<String, Value>,
    path: &str,
    schema_type: &str,
    tasks: &mut Vec<WalkTask<'a>>,
    violations: &mut Vec<String>,
) {
    let allowed_for: [(&str, [&str; 5]); 6] = [
        ("properties", ["object", "", "", "", ""]),
        ("required", ["object", "", "", "", ""]),
        ("additionalProperties", ["object", "", "", "", ""]),
        ("items", ["array", "", "", "", ""]),
        ("enum", ["string", "number", "integer", "boolean", "null"]),
        ("const", ["string", "number", "integer", "boolean", "null"]),
    ];
    for (key, types) in allowed_for {
        if record.contains_key(key) && !types.contains(&schema_type) {
            violations.push(format!(
                "{path}.{key} is not supported on type \"{schema_type}\""
            ));
        }
    }

    match schema_type {
        "object" => {
            let properties = record.get("properties");
            tasks.push(WalkTask::ObjectTail {
                node: record,
                path: path.to_owned(),
                properties,
            });
            if let Some(properties) = properties {
                if let Some(map) = properties.as_object() {
                    for (key, child) in map.iter().rev() {
                        tasks.push(WalkTask::Enter {
                            node: child,
                            path: format!("{path}.properties.{key}"),
                        });
                    }
                } else {
                    violations.push(format!("{path}.properties must be an object of schemas"));
                }
            }
        }
        "array" => {
            if let Some(items) = record.get("items") {
                tasks.push(WalkTask::Enter {
                    node: items,
                    path: format!("{path}.items"),
                });
            }
        }
        _ => {
            let has_enum = record.contains_key("enum");
            let enum_valid = has_enum
                && record
                    .get("enum")
                    .and_then(Value::as_array)
                    .is_some_and(|entries| {
                        !entries.is_empty()
                            && entries
                                .iter()
                                .all(|entry| scalar_matches(schema_type, entry))
                    });
            if has_enum && !enum_valid {
                violations.push(format!(
                    "{path}.enum must be a non-empty array of {schema_type} values"
                ));
            }
            let has_const = record.contains_key("const");
            if has_const {
                let declared_const = record.get("const").unwrap_or(&Value::Null);
                if !scalar_matches(schema_type, declared_const) {
                    violations.push(format!("{path}.const must be a {schema_type} value"));
                } else if enum_valid {
                    let allowed = record.get("enum").and_then(Value::as_array);
                    let in_enum = allowed.is_some_and(|entries| {
                        entries
                            .iter()
                            .any(|entry| json_scalar_eq(entry, declared_const))
                    });
                    if !in_enum {
                        violations.push(format!(
                            "{path}.const must be one of {path}.enum when both are declared"
                        ));
                    }
                }
            }
        }
    }
}

/// Collect every violation for one raw schema tree without using the call
/// stack (dsh `checkSchemaNode`).
fn check_schema_node(root: &Value, root_path: &str, violations: &mut Vec<String>) {
    let mut tasks = vec![WalkTask::Enter {
        node: root,
        path: root_path.to_owned(),
    }];
    while let Some(task) = tasks.pop() {
        match task {
            WalkTask::OneOfTail { node, path } => {
                for key in ONE_OF_SIBLING_KEYWORDS {
                    if node.contains_key(key) {
                        violations.push(format!("{path}.{key} is not supported beside oneOf"));
                    }
                }
            }
            WalkTask::ObjectTail {
                node,
                path,
                properties,
            } => check_object_schema_tail(node, &path, properties, violations),
            WalkTask::Enter { node, path } => {
                let Some(record) = node.as_object() else {
                    violations.push(format!("{path} must be a schema object"));
                    continue;
                };
                check_node_keys(record, &path, violations);

                let has_type = record.contains_key("type");
                let has_one_of = record.contains_key("oneOf");
                if has_type && has_one_of {
                    violations.push(format!("{path} cannot declare both type and oneOf"));
                    continue;
                }
                if !has_type && !has_one_of {
                    for key in ONE_OF_SIBLING_KEYWORDS {
                        if record.contains_key(key) {
                            violations.push(format!("{path}.{key} requires type or oneOf"));
                        }
                    }
                    continue;
                }

                if has_one_of {
                    check_one_of_node(record, &path, &mut tasks, violations);
                    continue;
                }

                let type_value = record.get("type").unwrap_or(&Value::Null);
                let Some(schema_type) = type_value.as_str() else {
                    if type_value.is_array() {
                        violations.push(format!(
                            "{path}.type must be a single type string (type arrays are \
not supported)"
                        ));
                    } else {
                        violations.push(format!(
                            "{path}.type must be one of {}",
                            SCHEMA_TYPES.join("/")
                        ));
                    }
                    continue;
                };
                if !SCHEMA_TYPES.contains(&schema_type) {
                    violations.push(format!(
                        "{path}.type must be one of {}",
                        SCHEMA_TYPES.join("/")
                    ));
                    continue;
                }
                check_typed_node(record, &path, schema_type, &mut tasks, violations);
            }
        }
    }
}

/// Assert that an arbitrary raw schema uses only the enforced subset.
///
/// Annotation-only schemas are accepted as the standard unconstrained-JSON
/// form; callers that require an object root use
/// [`assert_object_json_schema`].
///
/// # Errors
///
/// Returns a [`JsonSchemaError`] listing every violation when the schema
/// falls outside the subset.
pub fn assert_supported_json_schema(schema: &Value) -> Result<(), JsonSchemaError> {
    let mut violations = Vec::new();
    check_schema_node(schema, "schema", &mut violations);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(JsonSchemaError::new(violations))
    }
}

/// Assert the enforced subset plus the object-root constraint retained by
/// subagent and workflow structured outputs.
///
/// # Errors
///
/// Returns a [`JsonSchemaError`] when the schema is outside the subset or is
/// not object-rooted.
pub fn assert_object_json_schema(schema: &Value) -> Result<(), JsonSchemaError> {
    let mut violations = Vec::new();
    check_schema_node(schema, "schema", &mut violations);
    if violations.is_empty()
        && schema
            .as_object()
            .and_then(|record| record.get("type"))
            .and_then(Value::as_str)
            != Some("object")
    {
        violations
            .push("schema.type must be \"object\" (structured output is object-rooted)".to_owned());
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(JsonSchemaError::new(violations))
    }
}

/// One child evaluation deferred by a container or exact-one union frame.
struct ValueChild<'a> {
    node: &'a Value,
    value: &'a Value,
    path: String,
}

/// Container flavor of one validation frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// Exact-one union: children count matches.
    OneOf,
    /// Object: children walk declared properties.
    Object,
    /// Array: children walk items.
    Array,
}

/// Phase of one validation frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FramePhase {
    /// Type dispatch has not run yet.
    Start,
    /// Children are being validated.
    Children,
}

/// Explicit call frame for stack-free schema-value validation.
struct ValueFrame<'a> {
    node: &'a Value,
    value: &'a Value,
    path: String,
    kind: Option<FrameKind>,
    phase: FramePhase,
    children: Vec<ValueChild<'a>>,
    child_index: usize,
    violations: Vec<String>,
    tail_violations: Vec<String>,
    matches: usize,
}

impl<'a> ValueFrame<'a> {
    /// Initializes one frame with empty aggregation state.
    const fn new(node: &'a Value, value: &'a Value, path: String) -> Self {
        Self {
            node,
            value,
            path,
            kind: None,
            phase: FramePhase::Start,
            children: Vec::new(),
            child_index: 0,
            violations: Vec::new(),
            tail_violations: Vec::new(),
            matches: 0,
        }
    }
}

/// The root-frame diagnostic for a value outside the lossless JSON boundary.
fn lossless_value_violation(path: &str) -> Vec<String> {
    vec![format!(
        "\"{}\" must be a lossless JSON value",
        diagnostic_path(path)
    )]
}

/// Validate one scalar node after its primitive type check.
fn check_scalar_value(node: &Value, value: &Value, path: &str) -> Vec<String> {
    if let Some(allowed) = node.get("enum").and_then(Value::as_array)
        && !allowed.iter().any(|entry| json_scalar_eq(entry, value))
    {
        return vec![format!(
            "\"{}\" must be one of {}",
            diagnostic_path(path),
            js_scalar_array_to_string(allowed)
        )];
    }
    if let Some(declared) = node.get("const")
        && !json_scalar_eq(declared, value)
    {
        return vec![format!(
            "\"{}\" must be {}",
            diagnostic_path(path),
            js_scalar_to_string(declared)
        )];
    }
    Vec::new()
}

/// Hand one finished frame's result to its parent (or the root).
fn receive(
    frames: &mut [ValueFrame<'_>],
    result: Vec<String>,
    root_result: &mut Option<Vec<String>>,
) {
    match frames.last_mut() {
        None => *root_result = Some(result),
        Some(parent) => {
            if parent.kind == Some(FrameKind::OneOf) {
                if result.is_empty() {
                    parent.matches += 1;
                }
            } else {
                parent.violations.extend(result);
            }
        }
    }
}

/// Pop one finished frame and deliver its result.
fn finish(
    frames: &mut Vec<ValueFrame<'_>>,
    result: Vec<String>,
    root_result: &mut Option<Vec<String>>,
) {
    frames.pop();
    let rest = frames.as_mut_slice();
    receive(rest, result, root_result);
}

/// Start one object frame; a `Some` result finishes the frame immediately.
fn open_object_frame(frame: &mut ValueFrame<'_>, path: &str) -> Option<Vec<String>> {
    let node = frame.node;
    let value = frame.value;
    let record = node.as_object();
    let Some(record_value) = value.as_object() else {
        return Some(vec![format!(
            "\"{}\" must be an object",
            diagnostic_path(path)
        )]);
    };
    let properties = record
        .and_then(|r| r.get("properties"))
        .and_then(Value::as_object);
    let required = record
        .and_then(|r| r.get("required"))
        .and_then(Value::as_array);
    let mut violations = Vec::new();
    if let Some(required) = required {
        for key in required {
            let present = key
                .as_str()
                .is_some_and(|key| record_value.contains_key(key));
            if !present {
                let key = key.as_str().unwrap_or_default();
                violations.push(format!(
                    "missing required property \"{}\"",
                    property_path(path, key)
                ));
            }
        }
    }
    let mut children = Vec::new();
    if let Some(properties) = properties {
        for (key, child) in properties {
            if let Some(entry) = record_value.get(key) {
                children.push(ValueChild {
                    node: child,
                    value: entry,
                    path: property_path(path, key),
                });
            }
        }
    }
    let mut tail_violations = Vec::new();
    let closed = record
        .and_then(|r| r.get("additionalProperties"))
        .is_some_and(|flag| flag == &Value::Bool(false));
    if closed {
        let declared = properties;
        for key in record_value.keys() {
            let known = declared.is_some_and(|map| map.contains_key(key));
            if !known {
                tail_violations.push(format!(
                    "\"{}\" is not a declared property (additionalProperties: false)",
                    property_path(path, key)
                ));
            }
        }
    }
    frame.kind = Some(FrameKind::Object);
    frame.children = children;
    frame.violations = violations;
    frame.tail_violations = tail_violations;
    frame.phase = FramePhase::Children;
    None
}

/// Start one array frame; a `Some` result finishes the frame immediately.
fn open_array_frame(frame: &mut ValueFrame<'_>, path: &str) -> Option<Vec<String>> {
    let node = frame.node;
    let value = frame.value;
    let record = node.as_object();
    let Some(entries) = value.as_array() else {
        return Some(vec![format!(
            "\"{}\" must be an array",
            diagnostic_path(path)
        )]);
    };
    let items = record.and_then(|r| r.get("items"));
    let children = items
        .map(|items| {
            entries
                .iter()
                .enumerate()
                .map(|(index, entry)| ValueChild {
                    node: items,
                    value: entry,
                    path: format!("{path}[{index}]"),
                })
                .collect()
        })
        .unwrap_or_default();
    frame.kind = Some(FrameKind::Array);
    frame.children = children;
    frame.phase = FramePhase::Children;
    None
}

/// The primitive-shape violation of one scalar node, if any.
fn scalar_shape_violation(schema_type: SchemaType, value: &Value, path: &str) -> Option<String> {
    let violation = match schema_type {
        SchemaType::String if !value.is_string() => {
            format!("\"{}\" must be a string", diagnostic_path(path))
        }
        SchemaType::Number if !value.is_number() => {
            format!("\"{}\" must be a number", diagnostic_path(path))
        }
        SchemaType::Number if !is_json_number(value) => {
            format!("\"{}\" must be a finite JSON number", diagnostic_path(path))
        }
        SchemaType::Integer if !is_json_integer(value) => {
            format!("\"{}\" must be an integer", diagnostic_path(path))
        }
        SchemaType::Boolean if !value.is_boolean() => {
            format!("\"{}\" must be a boolean", diagnostic_path(path))
        }
        SchemaType::Null if !value.is_null() => {
            format!("\"{}\" must be null", diagnostic_path(path))
        }
        _ => return None,
    };
    Some(violation)
}

/// Validate one trusted schema/value pair with explicit frames rather than
/// recursive calls (dsh `checkValue`).
fn check_value(schema: &Value, value: &Value, path: &str) -> Vec<String> {
    let mut frames: Vec<ValueFrame<'_>> = vec![ValueFrame::new(schema, value, path.to_owned())];
    let mut root_result: Option<Vec<String>> = None;

    while !frames.is_empty() {
        let Some(frame) = frames.last_mut() else {
            break;
        };
        if frame.phase == FramePhase::Children {
            if frame.child_index < frame.children.len() {
                let child = &frame.children[frame.child_index];
                let (node, value) = (child.node, child.value);
                let child_path = child.path.clone();
                frame.child_index += 1;
                frames.push(ValueFrame::new(node, value, child_path));
                continue;
            }
            if frame.kind == Some(FrameKind::OneOf) {
                let matched = frame.matches;
                let path = frame.path.clone();
                let result = if matched == 1 {
                    Vec::new()
                } else {
                    vec![format!(
                        "\"{}\" must match exactly one oneOf branch (matched {matched})",
                        diagnostic_path(&path)
                    )]
                };
                finish(&mut frames, result, &mut root_result);
                continue;
            }
            let mut violations = std::mem::take(&mut frame.violations);
            violations.append(&mut frame.tail_violations);
            // The dsh lossless-object/array tail checks are structurally
            // satisfied: every serde_json value is lossless JSON.
            finish(&mut frames, violations, &mut root_result);
            continue;
        }

        let node = frame.node;
        let value = frame.value;
        let path = frame.path.clone();
        let record = node.as_object();
        let type_value = record.and_then(|r| r.get("type"));
        let schema_type = type_value.map(|value| {
            SchemaType::of(value).unwrap_or_else(|| unreachable!("schema type outside subset")) // unreachable by contract
        });
        if record.is_some_and(|r| r.contains_key("oneOf")) {
            frame.kind = Some(FrameKind::OneOf);
            let branches = node
                .get("oneOf")
                .and_then(Value::as_array)
                .unwrap_or_else(|| unreachable!("oneOf asserted array")); // unreachable by contract
            frame.children = branches
                .iter()
                .map(|branch| ValueChild {
                    node: branch,
                    value,
                    path: path.clone(),
                })
                .collect();
            frame.child_index = 0;
            frame.matches = 0;
            frame.phase = FramePhase::Children;
            continue;
        }
        let Some(schema_type) = schema_type else {
            // Annotation-only node: any lossless JSON value is valid.
            finish(&mut frames, Vec::new(), &mut root_result);
            continue;
        };

        let immediate = match schema_type {
            SchemaType::Object => open_object_frame(frame, &path),
            SchemaType::Array => open_array_frame(frame, &path),
            _ => Some(
                scalar_shape_violation(schema_type, value, &path).map_or_else(
                    || check_scalar_value(node, value, &path),
                    |violation| vec![violation],
                ),
            ),
        };
        if let Some(result) = immediate {
            finish(&mut frames, result, &mut root_result);
        }
    }

    root_result.unwrap_or_else(|| lossless_value_violation(path))
}

/// Validate a candidate value against an asserted raw schema. The function is
/// total for arbitrary values and returns path-qualified violations.
///
/// The schema must have passed [`assert_supported_json_schema`]; the walk
/// trusts that assertion.
#[must_use]
pub fn validate_json_schema_value(schema: &Value, value: &Value, path: &str) -> Vec<String> {
    check_value(schema, value, path)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn violations_of(schema: &Value) -> Vec<String> {
        assert_supported_json_schema(schema).unwrap_err().violations
    }

    #[test]
    fn rejects_unknown_keyword() {
        let violations = violations_of(&json!({"type": "string", "minLength": 1}));
        assert_eq!(
            violations,
            vec![
                "schema.minLength is not a supported keyword (subset: \
type/oneOf/properties/required/additionalProperties/items/enum/const + annotations)"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn rejects_type_array_and_unknown_type() {
        assert_eq!(
            violations_of(&json!({"type": ["string", "null"]})),
            vec![
                "schema.type must be a single type string (type arrays are not supported)"
                    .to_owned()
            ]
        );
        assert_eq!(
            violations_of(&json!({"type": "any"})),
            vec![
                "schema.type must be one of object/array/string/number/integer/boolean/null"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn rejects_type_beside_one_of() {
        assert_eq!(
            violations_of(&json!({"type": "string", "oneOf": [{}, {}]})),
            vec!["schema cannot declare both type and oneOf".to_owned()]
        );
    }

    #[test]
    fn rejects_one_of_siblings_and_short_branches() {
        let violations = violations_of(&json!({"oneOf": [{"type": "string"}], "items": {}}));
        assert_eq!(
            violations,
            vec![
                "schema.oneOf must be an array of at least two schemas".to_owned(),
                "schema.items is not supported beside oneOf".to_owned()
            ]
        );
        let two = json!({
            "oneOf": [{"type": "string"}, {"type": "number"}],
            "properties": {"a": {}}
        });
        assert_eq!(
            violations_of(&two),
            vec!["schema.properties is not supported beside oneOf".to_owned()]
        );
    }

    #[test]
    fn rejects_keyword_without_type_or_one_of() {
        assert_eq!(
            violations_of(&json!({"items": {}})),
            vec!["schema.items requires type or oneOf".to_owned()]
        );
    }

    #[test]
    fn rejects_misplaced_keyword() {
        assert_eq!(
            violations_of(&json!({"type": "string", "items": {}})),
            vec!["schema.items is not supported on type \"string\"".to_owned()]
        );
    }

    #[test]
    fn rejects_required_not_declared() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a", "b"]
        });
        assert_eq!(
            violations_of(&schema),
            vec!["schema.required names \"b\" which is not in properties".to_owned()]
        );
        let bad = json!({"type": "object", "required": "a"});
        assert_eq!(
            violations_of(&bad),
            vec!["schema.required must be an array of strings".to_owned()]
        );
    }

    #[test]
    fn rejects_non_boolean_additional_properties() {
        assert_eq!(
            violations_of(&json!({"type": "object", "additionalProperties": "no"})),
            vec!["schema.additionalProperties must be a boolean".to_owned()]
        );
    }

    #[test]
    fn rejects_bad_enum_and_const() {
        assert_eq!(
            violations_of(&json!({"type": "string", "enum": []})),
            vec!["schema.enum must be a non-empty array of string values".to_owned()]
        );
        assert_eq!(
            violations_of(&json!({"type": "number", "enum": ["x"]})),
            vec!["schema.enum must be a non-empty array of number values".to_owned()]
        );
        assert_eq!(
            violations_of(&json!({"type": "number", "const": "x"})),
            vec!["schema.const must be a number value".to_owned()]
        );
        let both = json!({"type": "string", "enum": ["a", "b"], "const": "c"});
        assert_eq!(
            violations_of(&both),
            vec!["schema.const must be one of schema.enum when both are declared".to_owned()]
        );
    }

    #[test]
    fn rejects_non_string_annotations_and_non_schema_nodes() {
        assert_eq!(
            violations_of(&json!({"type": "string", "description": 3})),
            vec!["schema.description must be a string".to_owned()]
        );
        assert_eq!(
            violations_of(&json!({"type": "string", "title": 3})),
            vec!["schema.title must be a string".to_owned()]
        );
        assert_eq!(
            violations_of(&Value::String("x".into())),
            vec!["schema must be a schema object".to_owned()]
        );
    }

    #[test]
    fn rejects_properties_not_object_and_nested_paths() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "str"}}
        });
        assert_eq!(
            violations_of(&schema),
            vec![
                "schema.properties.a.type must be one of \
object/array/string/number/integer/boolean/null"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn object_root_constraint() {
        let err = assert_object_json_schema(&json!({"type": "string"})).unwrap_err();
        assert_eq!(
            err.violations,
            vec!["schema.type must be \"object\" (structured output is object-rooted)".to_owned()]
        );
        assert_object_json_schema(&json!({"type": "object"})).unwrap();
    }

    #[test]
    fn error_display_joins_violations() {
        let err = assert_supported_json_schema(&json!({"nope": 1, "alsoBad": 2})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unsupported JSON schema: schema.nope is not a supported keyword (subset: \
type/oneOf/properties/required/additionalProperties/items/enum/const + annotations); \
schema.alsoBad is not a supported keyword (subset: type/oneOf/properties/required/\
additionalProperties/items/enum/const + annotations)"
        );
    }

    #[test]
    fn accepts_valid_subset_shapes() {
        let schema = json!({
            "type": "object",
            "description": "root",
            "title": "Root",
            "default": {"a": 1},
            "examples": [1, "x", null, true],
            "properties": {
                "a": {"type": "string", "enum": ["x", "y"], "const": "x"},
                "b": {
                    "type": "array",
                    "items": {"oneOf": [{"type": "null"}, {"type": "boolean"}]}
                },
                "c": {"type": "object", "additionalProperties": true}
            },
            "required": ["a"],
            "additionalProperties": false
        });
        assert_supported_json_schema(&schema).unwrap();
        assert_supported_json_schema(&json!({"description": "any"})).unwrap();
    }

    #[test]
    fn validate_reports_every_family() {
        let schema = json!({
            "type": "object",
            "properties": {
                "s": {"type": "string"},
                "n": {"type": "number", "enum": [2.0, 3.5]},
                "i": {"type": "integer"},
                "b": {"type": "boolean"},
                "z": {"type": "null"},
                "a": {"type": "array", "items": {"type": "string"}},
                "o": {"type": "object", "additionalProperties": false},
                "u": {"oneOf": [{"type": "string"}, {"type": "boolean"}]}
            },
            "required": ["s", "n"]
        });
        let value = json!({
            "n": "no",
            "i": 1.5,
            "b": 1,
            "z": 0,
            "a": [1],
            "o": {"x": 1},
            "u": 5
        });
        let violations = validate_json_schema_value(&schema, &value, "value");
        assert_eq!(
            violations,
            vec![
                "missing required property \"value.s\"",
                "\"value.n\" must be a number",
                "\"value.i\" must be an integer",
                "\"value.b\" must be a boolean",
                "\"value.z\" must be null",
                "\"value.a[0]\" must be a string",
                "\"value.o.x\" is not a declared property (additionalProperties: false)",
                "\"value.u\" must match exactly one oneOf branch (matched 0)"
            ]
        );
    }

    #[test]
    fn validate_scalar_and_root_diagnostics() {
        let string_schema = json!({"type": "string"});
        assert_eq!(
            validate_json_schema_value(&string_schema, &json!(1), "value"),
            vec!["\"value\" must be a string".to_owned()]
        );
        let enum_schema = json!({"type": "number", "enum": [2.0, 3.5]});
        assert_eq!(
            validate_json_schema_value(&enum_schema, &json!(7), "value"),
            vec!["\"value\" must be one of [2,3.5]".to_owned()]
        );
        let const_schema = json!({"type": "string", "const": "here"});
        assert_eq!(
            validate_json_schema_value(&const_schema, &json!("there"), "value"),
            vec!["\"value\" must be \"here\"".to_owned()]
        );
        // Numeric equality follows the JS number domain: 2 equals 2.0.
        assert_eq!(
            validate_json_schema_value(&enum_schema, &json!(2), "value"),
            [] as [std::string::String; 0]
        );
        let required_schema =
            json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]});
        assert_eq!(
            validate_json_schema_value(&required_schema, &json!({}), "value"),
            vec!["missing required property \"value.a\"".to_owned()]
        );
    }

    #[test]
    fn validate_uses_arguments_label_at_empty_root() {
        let schema =
            json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]});
        assert_eq!(
            validate_json_schema_value(&schema, &json!({"a": 1}), ""),
            vec!["\"a\" must be a string".to_owned()]
        );
        assert_eq!(
            validate_json_schema_value(&schema, &json!({}), ""),
            vec!["missing required property \"a\"".to_owned()]
        );
        let array_schema = json!({"type": "array", "items": {"type": "string"}});
        assert_eq!(
            validate_json_schema_value(&array_schema, &json!([1]), ""),
            vec!["\"[0]\" must be a string".to_owned()]
        );
    }

    #[test]
    fn validate_rejects_negative_zero_and_accepts_float_integers() {
        let number_schema = json!({"type": "number"});
        let neg_zero: Value = serde_json::from_str("-0.0").expect("negative zero parses as f64");
        assert_eq!(
            validate_json_schema_value(&number_schema, &neg_zero, "value"),
            vec!["\"value\" must be a finite JSON number".to_owned()]
        );
        let integer_schema = json!({"type": "integer"});
        assert_eq!(
            validate_json_schema_value(&integer_schema, &json!(2.0), "value"),
            [] as [std::string::String; 0]
        );
        assert_eq!(
            validate_json_schema_value(&integer_schema, &json!(2.5), "value"),
            vec!["\"value\" must be an integer".to_owned()]
        );
    }

    #[test]
    fn validate_one_of_branch_violations_are_counted_not_listed() {
        let schema = json!({
            "oneOf": [
                {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
                {"type": "object", "properties": {"b": {"type": "string"}}, "required": ["b"]}
            ]
        });
        let violations = validate_json_schema_value(&schema, &json!({"a": 1}), "value");
        assert_eq!(
            violations,
            vec!["\"value\" must match exactly one oneOf branch (matched 0)".to_owned()]
        );
        assert_eq!(
            validate_json_schema_value(&schema, &json!({"a": "x"}), "value"),
            [] as [std::string::String; 0]
        );
        let both = validate_json_schema_value(&schema, &json!({"a": "x", "b": "y"}), "value");
        assert_eq!(
            both,
            vec!["\"value\" must match exactly one oneOf branch (matched 2)".to_owned()]
        );
    }

    #[test]
    fn walks_deep_schemas_without_recursion() {
        let depth = 20_000;
        let mut schema = json!({"type": "string"});
        let mut value = json!("leaf");
        for _ in 0..depth {
            // Build by moving the previous tree in; `json!` interpolation
            // would deep-copy recursively (`to_value`) and overflow first.
            let mut wrapper = Map::new();
            wrapper.insert("type".to_owned(), json!("array"));
            wrapper.insert("items".to_owned(), schema);
            schema = Value::Object(wrapper);
            value = Value::Array(vec![value]);
        }
        assert_supported_json_schema(&schema).unwrap();
        assert_eq!(
            validate_json_schema_value(&schema, &value, "value"),
            [] as [std::string::String; 0]
        );
        let mut bad_wrapper = Map::new();
        bad_wrapper.insert("type".to_owned(), json!("array"));
        bad_wrapper.insert("items".to_owned(), schema);
        let bad = Value::Object(bad_wrapper);
        assert_eq!(
            validate_json_schema_value(&bad, &json!([[[["x"]]]]), "value").len(),
            1
        );
        // serde_json's recursive Drop overflows the stack at this depth; the
        // iterative walks above already proved their shape, so leak the deep
        // trees instead of dropping them (an inherent serde_json boundary,
        // recorded in findings).
        std::mem::forget(bad);
        std::mem::forget(value);
    }
}
