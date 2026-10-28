// Rust guideline compliant 2026-10-28
//! Author-facing value-schema DSL, iterative compilation to the enforced raw
//! JSON Schema subset, and typed argument validation (dsh
//! `core/tools/src/schema.ts`, verbatim semantics).
//!
//! The author DSL is a JSON object vocabulary: nine node shapes
//! (`string`/`number`/`integer`/`boolean`/`null`/`array`/`object`/`json`/
//! `oneOf`) with four annotation keys, `object` nodes must declare an
//! explicit boolean `additionalProperties`, and unknown keys reject naming
//! their path. Compilation is frame-iterative (the stack safety of the dsh
//! task graph); where dsh attaches partially-built nodes by reference, this
//! port keeps each partial node in its build frame and delivers it upward on
//! completion. A property-position node admits the `required` key (the dsh
//! `allowRequired` task flag) — derived here from the delivery point, since
//! only property-map frames push property attachments. Cycle detection is
//! structurally absent: a `serde_json::Value` tree cannot alias itself (the
//! value-semantics judgment, findings slice 7).

use serde_json::{Map, Value};

use crate::json_schema::{
    JsonSchemaError, assert_supported_json_schema, validate_json_schema_value,
};

/// Error code of the typed-argument boundary (dsh `ToolArgsError`).
pub const INVALID_ARGS_CODE: &str = "INVALID_ARGS";

/// Annotation keys shared by every author-facing schema node.
const ANNOTATION_KEYS: [&str; 4] = ["description", "title", "default", "examples"];

/// Invalid model-generated arguments for a typed tool (dsh `ToolArgsError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolArgsError {
    /// Individual violations in schema-walk order.
    pub violations: Vec<String>,
}

impl ToolArgsError {
    /// Wraps the collected violation texts.
    #[must_use]
    pub const fn new(violations: Vec<String>) -> Self {
        Self { violations }
    }
}

impl std::fmt::Display for ToolArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid arguments: {}", self.violations.join("; "))
    }
}

impl std::error::Error for ToolArgsError {}

/// Throw one author-schema violation through the shared schema error type.
fn author_error(message: String) -> JsonSchemaError {
    JsonSchemaError::new(vec![message])
}

/// Reject author-only keys outside one node's declared vocabulary; the first
/// offending key in declaration order rejects.
fn assert_author_keys(
    record: &Map<String, Value>,
    path: &str,
    allowed: &[&str],
) -> Result<(), JsonSchemaError> {
    for key in record.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(author_error(format!(
                "{path}.{key} is not supported by the value schema DSL"
            )));
        }
    }
    Ok(())
}

/// Copy own annotation fields for validation by the raw-schema boundary.
fn copy_annotations(source: &Map<String, Value>, target: &mut Map<String, Value>) {
    for key in ANNOTATION_KEYS {
        if let Some(value) = source.get(key) {
            target.insert(key.to_owned(), value.clone());
        }
    }
}

/// Where one completed build product is delivered.
#[derive(Debug, Clone)]
enum Attach {
    /// The machine root; its product ends the run.
    Root,
    /// A property value delivered into a property-map frame. Only
    /// property-map frames push this attachment, so it also marks the one
    /// position whose vocabulary admits `required` (dsh `allowRequired`).
    Property { key: String },
    /// An `items` child delivered into its array node frame.
    Items,
    /// One `oneOf` branch delivered into its union node frame.
    Branch { index: usize },
    /// A compiled property map delivered into its object node frame.
    MapProperties,
}

/// One deferred child of a node frame, in declaration order.
#[derive(Clone)]
enum PendingChild<'a> {
    /// Compile one value node and deliver it to its attachment.
    Value {
        input: &'a Value,
        path: String,
        attach: Attach,
    },
    /// Compile one property map into the containing object node.
    PropertyMap { input: &'a Value, path: String },
}

/// The completed product of one build frame.
enum Product {
    /// A compiled schema node.
    Node(Value),
    /// A compiled property map with its collected required names.
    Map {
        properties: Map<String, Value>,
        required: Vec<String>,
    },
}

/// One explicit frame of the author-schema compiler.
enum BuildFrame<'a> {
    /// Compiles one value node; the arm dispatch fills `node` and `pending`.
    Node {
        input: &'a Value,
        path: String,
        attach: Attach,
        node: Option<Value>,
        pending: Vec<PendingChild<'a>>,
        pending_index: usize,
    },
    /// Compiles one implicit property map.
    Map {
        path: String,
        properties: Map<String, Value>,
        required: Vec<String>,
        pending: Vec<(&'a str, &'a Value)>,
        pending_index: usize,
    },
}

impl<'a> BuildFrame<'a> {
    /// Starts one node frame.
    const fn node(input: &'a Value, path: String, attach: Attach) -> Self {
        Self::Node {
            input,
            path,
            attach,
            node: None,
            pending: Vec::new(),
            pending_index: 0,
        }
    }

    /// Starts one property-map frame over an already-verified record.
    fn map(record: &'a Map<String, Value>, path: String) -> Self {
        Self::Map {
            path,
            properties: Map::new(),
            required: Vec::new(),
            pending: record.iter().map(|(k, v)| (k.as_str(), v)).collect(),
            pending_index: 0,
        }
    }
}

/// Dispatch the exact-one union arm (dsh `oneOf` author branch).
fn dispatch_one_of<'a>(
    record: &'a Map<String, Value>,
    path: &str,
    mut allowed: Vec<&'a str>,
) -> Result<(Value, Vec<PendingChild<'a>>), JsonSchemaError> {
    allowed.push("oneOf");
    allowed.push("type");
    assert_author_keys(record, path, &allowed)?;
    if record.contains_key("type") {
        return Err(author_error(format!(
            "{path} cannot declare both type and oneOf"
        )));
    }
    let one_of = record.get("oneOf").unwrap_or(&Value::Null);
    if !one_of.is_array() {
        return Err(author_error(format!(
            "{path}.oneOf must be an array of at least two value schemas"
        )));
    }
    let branch_count = one_of.as_array().map_or(0, Vec::len);
    let mut node = Map::new();
    node.insert(
        "oneOf".to_owned(),
        Value::Array(vec![Value::Null; branch_count]),
    );
    copy_annotations(record, &mut node);
    let mut pending: Vec<PendingChild<'a>> = Vec::new();
    for (index, branch) in one_of.as_array().into_iter().flatten().enumerate() {
        pending.push(PendingChild::Value {
            input: branch,
            path: format!("{path}.oneOf[{index}]"),
            attach: Attach::Branch { index },
        });
    }
    Ok((Value::Object(node), pending))
}

/// Dispatch one node frame's author arm: validate keys, build the base node,
/// and queue its children in declaration order.
fn dispatch_node<'a>(
    record: &'a Map<String, Value>,
    path: &str,
    allow_required: bool,
) -> Result<(Value, Vec<PendingChild<'a>>), JsonSchemaError> {
    let mut allowed: Vec<&str> = ANNOTATION_KEYS.to_vec();
    if allow_required {
        allowed.push("required");
    }
    let mut node = Map::new();
    let mut pending: Vec<PendingChild<'_>> = Vec::new();

    if record.contains_key("oneOf") {
        return dispatch_one_of(record, path, allowed);
    }

    let input_type = record.get("type").and_then(Value::as_str);
    match input_type {
        Some("json") => {
            allowed.push("type");
            assert_author_keys(record, path, &allowed)?;
            copy_annotations(record, &mut node);
        }
        Some("object") => {
            for key in ["type", "properties", "additionalProperties"] {
                allowed.push(key);
            }
            assert_author_keys(record, path, &allowed)?;
            let explicit = record.get("additionalProperties").and_then(Value::as_bool);
            if explicit.is_none() {
                return Err(author_error(format!(
                    "{path}.additionalProperties must be explicitly true or false"
                )));
            }
            node.insert("type".to_owned(), Value::String("object".to_owned()));
            copy_annotations(record, &mut node);
            node.insert(
                "additionalProperties".to_owned(),
                Value::Bool(explicit.unwrap_or(false)),
            );
            if let Some(properties) = record.get("properties") {
                pending.push(PendingChild::PropertyMap {
                    input: properties,
                    path: format!("{path}.properties"),
                });
            }
        }
        Some("array") => {
            allowed.push("type");
            allowed.push("items");
            assert_author_keys(record, path, &allowed)?;
            node.insert("type".to_owned(), Value::String("array".to_owned()));
            copy_annotations(record, &mut node);
            if let Some(items) = record.get("items") {
                pending.push(PendingChild::Value {
                    input: items,
                    path: format!("{path}.items"),
                    attach: Attach::Items,
                });
            }
        }
        Some(scalar_type @ ("string" | "number" | "integer" | "boolean" | "null")) => {
            allowed.push("type");
            allowed.push("enum");
            allowed.push("const");
            assert_author_keys(record, path, &allowed)?;
            node.insert("type".to_owned(), Value::String(scalar_type.to_owned()));
            copy_annotations(record, &mut node);
            if let Some(values) = record.get("enum") {
                if !values.is_array() {
                    return Err(author_error(format!(
                        "{path}.enum must be a non-empty array of scalar values"
                    )));
                }
                node.insert("enum".to_owned(), values.clone());
            }
            if let Some(value) = record.get("const") {
                node.insert("const".to_owned(), value.clone());
            }
        }
        _ => {
            return Err(author_error(format!(
                "{path}.type must be string/number/integer/boolean/null/array/object/json, \
or use oneOf"
            )));
        }
    }
    Ok((Value::Object(node), pending))
}

/// Deliver one completed product to its parent frame (or the machine root).
fn deliver(
    frames: &mut [BuildFrame<'_>],
    attach: Attach,
    product: Product,
    root: &mut Option<Product>,
) {
    let Some(parent) = frames.last_mut() else {
        *root = Some(product);
        return;
    };
    match (parent, attach, product) {
        (BuildFrame::Map { properties, .. }, Attach::Property { key }, Product::Node(node)) => {
            properties.insert(key, node);
        }
        (BuildFrame::Node { node, .. }, Attach::Items, Product::Node(child)) => {
            let target = node.as_mut().unwrap_or_else(|| {
                unreachable!("node base is built at dispatch") // unreachable by construction
            });
            target["items"] = child;
        }
        (BuildFrame::Node { node, .. }, Attach::Branch { index }, Product::Node(child)) => {
            let target = node.as_mut().unwrap_or_else(|| {
                unreachable!("node base is built at dispatch") // unreachable by construction
            });
            target["oneOf"][index] = child;
        }
        (
            BuildFrame::Node { node, .. },
            Attach::MapProperties,
            Product::Map {
                properties,
                required,
            },
        ) => {
            let target = node.as_mut().unwrap_or_else(|| {
                unreachable!("node base is built at dispatch") // unreachable by construction
            });
            target["properties"] = Value::Object(properties);
            if !required.is_empty() {
                target["required"] =
                    Value::Array(required.into_iter().map(Value::String).collect());
            }
        }
        (_, attach, _) => {
            unreachable!("attach {attach:?} has no target frame"); // unreachable by construction
        }
    }
}

/// Push one property-map child frame after the dsh record check.
fn push_map_frame<'a>(
    frames: &mut Vec<BuildFrame<'a>>,
    input: &'a Value,
    path: String,
) -> Result<(), JsonSchemaError> {
    let Some(record) = input.as_object() else {
        return Err(author_error(format!(
            "{path} must be an object of value schemas"
        )));
    };
    frames.push(BuildFrame::map(record, path));
    Ok(())
}

/// Execute the author-schema compiler without recursive descent; returns the
/// root product.
fn run_compiler(initial: BuildFrame<'_>) -> Result<Product, JsonSchemaError> {
    let mut frames: Vec<BuildFrame<'_>> = vec![initial];
    let mut root: Option<Product> = None;
    while !frames.is_empty() {
        let Some(frame) = frames.last_mut() else {
            break;
        };
        match frame {
            BuildFrame::Node {
                input,
                path,
                attach,
                node,
                pending,
                pending_index,
            } => {
                if node.is_none() {
                    let input: &Value = input;
                    let record = input.as_object().ok_or_else(|| {
                        author_error(format!("{path} must be a value schema object"))
                    })?;
                    let allow_required = matches!(attach, Attach::Property { .. });
                    let (base, children) = dispatch_node(record, path, allow_required)?;
                    *node = Some(base);
                    *pending = children;
                    *pending_index = 0;
                    continue;
                }
                if *pending_index < pending.len() {
                    let child = pending[*pending_index].clone();
                    *pending_index += 1;
                    match child {
                        PendingChild::Value {
                            input,
                            path,
                            attach,
                        } => {
                            frames.push(BuildFrame::node(input, path, attach));
                        }
                        PendingChild::PropertyMap { input, path } => {
                            push_map_frame(&mut frames, input, path)?;
                        }
                    }
                    continue;
                }
                let attach = attach.clone();
                let product = Product::Node(node.take().unwrap_or_else(|| {
                    unreachable!("node base is built at dispatch") // unreachable by construction
                }));
                frames.pop();
                deliver(frames.as_mut_slice(), attach, product, &mut root);
            }
            BuildFrame::Map {
                path,
                properties,
                required,
                pending,
                pending_index,
            } => {
                if *pending_index < pending.len() {
                    let (key, property) = pending[*pending_index];
                    *pending_index += 1;
                    let record = property.as_object().ok_or_else(|| {
                        author_error(format!("{path}.{key} must be a value schema object"))
                    })?;
                    if let Some(flag) = record.get("required") {
                        if flag.as_bool() != Some(true) {
                            return Err(author_error(format!(
                                "{path}.{key}.required must be true when present"
                            )));
                        }
                        required.push(key.to_owned());
                    }
                    let attach = Attach::Property {
                        key: key.to_owned(),
                    };
                    let child_path = format!("{path}.{key}");
                    frames.push(BuildFrame::node(property, child_path, attach));
                    continue;
                }
                let product = Product::Map {
                    properties: std::mem::take(properties),
                    required: std::mem::take(required),
                };
                frames.pop();
                deliver(
                    frames.as_mut_slice(),
                    Attach::MapProperties,
                    product,
                    &mut root,
                );
            }
        }
    }
    root.ok_or_else(|| author_error("schema did not compile".to_owned()))
}

/// Compile one author node without applying any consumer root restriction.
fn compile_value_schema(input: &Value, path: &str) -> Result<Value, JsonSchemaError> {
    match run_compiler(BuildFrame::node(input, path.to_owned(), Attach::Root))? {
        Product::Node(node) => Ok(node),
        Product::Map { .. } => Err(author_error(format!("{path} did not compile"))),
    }
}

/// Compile one implicit property map, collecting per-property requiredness.
fn compile_property_map(
    input: &Value,
    path: &str,
) -> Result<(Map<String, Value>, Vec<String>), JsonSchemaError> {
    let Some(record) = input.as_object() else {
        return Err(author_error(format!(
            "{path} must be an object of value schemas"
        )));
    };
    match run_compiler(BuildFrame::map(record, path.to_owned()))? {
        Product::Map {
            properties,
            required,
        } => Ok((properties, required)),
        Product::Node(_) => Err(author_error(format!("{path} did not compile"))),
    }
}

/// Compile one author-facing value schema to the enforced raw JSON Schema
/// subset. The author-only `json` node becomes an annotation-only schema.
///
/// # Errors
///
/// Returns the first author violation, or every raw-subset violation of the
/// compiled projection.
pub fn value_schema_spec_to_json_schema(spec: &Value) -> Result<Value, JsonSchemaError> {
    let schema = compile_value_schema(spec, "schema")?;
    assert_supported_json_schema(&schema)?;
    Ok(schema)
}

/// Compile the implicit open parameter object into raw JSON Schema: an
/// object root with no `additionalProperties` override (JSON Schema's open
/// default).
///
/// # Errors
///
/// Returns the first author violation, or every raw-subset violation of the
/// compiled projection.
pub fn parameter_schema_spec_to_json_schema(spec: &Value) -> Result<Value, JsonSchemaError> {
    let (properties, required) = compile_property_map(spec, "parameters")?;
    let mut root = Map::new();
    root.insert("type".to_owned(), Value::String("object".to_owned()));
    root.insert("properties".to_owned(), Value::Object(properties));
    if !required.is_empty() {
        root.insert(
            "required".to_owned(),
            Value::Array(required.into_iter().map(Value::String).collect()),
        );
    }
    let schema = Value::Object(root);
    assert_supported_json_schema(&schema)?;
    Ok(schema)
}

/// Validate model-generated arguments against an implicit parameter schema.
///
/// # Errors
///
/// Returns the compilation failure when the spec itself is invalid; the
/// returned violations are empty exactly when the arguments validate.
pub fn validate_args(spec: &Value, args: &Value) -> Result<Vec<String>, JsonSchemaError> {
    let schema = parameter_schema_spec_to_json_schema(spec)?;
    Ok(validate_json_schema_value(&schema, args, ""))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn author_violation(spec: &Value) -> String {
        value_schema_spec_to_json_schema(spec)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn compiles_scalar_and_json_nodes() {
        let schema = value_schema_spec_to_json_schema(&json!({
            "type": "string",
            "description": "one name",
            "enum": ["a", "b"],
            "const": "a"
        }))
        .unwrap();
        assert_eq!(
            schema,
            json!({
                "type": "string",
                "description": "one name",
                "enum": ["a", "b"],
                "const": "a"
            })
        );
        let any = value_schema_spec_to_json_schema(&json!({"type": "json"})).unwrap();
        assert_eq!(any, json!({}));
        let annotated = value_schema_spec_to_json_schema(&json!({
            "type": "json",
            "title": "Any",
            "default": 1,
            "examples": [1]
        }))
        .unwrap();
        assert_eq!(
            annotated,
            json!({"title": "Any", "default": 1, "examples": [1]})
        );
    }

    #[test]
    fn compiles_nested_containers_with_wire_key_order() {
        let schema = value_schema_spec_to_json_schema(&json!({
            "type": "object",
            "description": "root",
            "additionalProperties": false,
            "properties": {
                "list": {
                    "type": "array",
                    "title": "List",
                    "items": {"type": "number"}
                },
                "pick": {
                    "oneOf": [{"type": "string"}, {"type": "boolean"}],
                    "description": "either"
                },
                "closed": {
                    "type": "object",
                    "additionalProperties": true
                }
            }
        }))
        .unwrap();
        let root = schema.as_object().unwrap();
        assert_eq!(
            root.keys().collect::<Vec<_>>(),
            vec!["type", "description", "additionalProperties", "properties"]
        );
        let properties = root.get("properties").and_then(Value::as_object).unwrap();
        let list = properties.get("list").and_then(Value::as_object).unwrap();
        assert_eq!(
            list.keys().collect::<Vec<_>>(),
            vec!["type", "title", "items"]
        );
        let pick = properties.get("pick").and_then(Value::as_object).unwrap();
        assert_eq!(
            pick.keys().collect::<Vec<_>>(),
            vec!["oneOf", "description"]
        );
    }

    #[test]
    fn one_of_branch_properties_collect_required() {
        let schema = value_schema_spec_to_json_schema(&json!({
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "a": {"type": "string", "description": "A", "required": true}
                    }
                },
                {"type": "boolean"}
            ]
        }))
        .unwrap();
        assert_eq!(
            schema,
            json!({
                "oneOf": [
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"a": {"type": "string", "description": "A"}},
                        "required": ["a"]
                    },
                    {"type": "boolean"}
                ]
            })
        );
    }

    #[test]
    fn author_rejects_unknown_keys() {
        assert_eq!(
            author_violation(&json!({"type": "string", "minLength": 1})),
            "unsupported JSON schema: schema.minLength is not supported by the value schema DSL"
        );
    }

    #[test]
    fn author_rejects_implicit_openness() {
        assert_eq!(
            author_violation(&json!({"type": "object"})),
            "unsupported JSON schema: schema.additionalProperties must be explicitly true or \
false"
        );
        assert_eq!(
            author_violation(&json!({"type": "object", "additionalProperties": "yes"})),
            "unsupported JSON schema: schema.additionalProperties must be explicitly true or \
false"
        );
    }

    #[test]
    fn author_rejects_type_and_one_of_together() {
        assert_eq!(
            author_violation(&json!({
                "type": "string",
                "oneOf": [{"type": "string"}, {"type": "null"}]
            })),
            "unsupported JSON schema: schema cannot declare both type and oneOf"
        );
    }

    #[test]
    fn author_one_of_length_is_subset_enforced() {
        // A non-array oneOf rejects at the author layer with the DSL text.
        assert_eq!(
            author_violation(&json!({"oneOf": {"type": "string"}})),
            "unsupported JSON schema: schema.oneOf must be an array of at least two value \
schemas"
        );
        // A short array passes the author layer and rejects at the subset
        // boundary with the subset text.
        assert_eq!(
            author_violation(&json!({"oneOf": [{"type": "string"}]})),
            "unsupported JSON schema: schema.oneOf must be an array of at least two schemas"
        );
    }

    #[test]
    fn author_rejects_type_vocabulary() {
        assert_eq!(
            author_violation(&json!({"type": "any"})),
            "unsupported JSON schema: schema.type must be string/number/integer/boolean/null/\
array/object/json, or use oneOf"
        );
        assert_eq!(
            author_violation(&json!({})),
            "unsupported JSON schema: schema.type must be string/number/integer/boolean/null/\
array/object/json, or use oneOf"
        );
    }

    #[test]
    fn author_rejects_non_object_shapes() {
        assert_eq!(
            author_violation(&json!("nope")),
            "unsupported JSON schema: schema must be a value schema object"
        );
        assert_eq!(
            parameter_schema_spec_to_json_schema(&json!([]))
                .unwrap_err()
                .to_string(),
            "unsupported JSON schema: parameters must be an object of value schemas"
        );
        assert_eq!(
            parameter_schema_spec_to_json_schema(&json!({"a": 3}))
                .unwrap_err()
                .to_string(),
            "unsupported JSON schema: parameters.a must be a value schema object"
        );
        assert_eq!(
            author_violation(&json!({
                "type": "object",
                "additionalProperties": false,
                "properties": 3
            })),
            "unsupported JSON schema: schema.properties must be an object of value schemas"
        );
    }

    #[test]
    fn author_enum_length_is_subset_enforced() {
        // An empty enum passes the author layer; the subset rejects it.
        assert_eq!(
            author_violation(&json!({"type": "string", "enum": []})),
            "unsupported JSON schema: schema.enum must be a non-empty array of string values"
        );
        // A non-array enum rejects at the author layer.
        assert_eq!(
            author_violation(&json!({"type": "string", "enum": "a"})),
            "unsupported JSON schema: schema.enum must be a non-empty array of scalar values"
        );
        // Type-mismatched entries pass the author layer; the subset rejects.
        assert_eq!(
            author_violation(&json!({"type": "number", "enum": ["x"]})),
            "unsupported JSON schema: schema.enum must be a non-empty array of number values"
        );
    }

    #[test]
    fn parameter_root_is_implicitly_open() {
        let schema = parameter_schema_spec_to_json_schema(&json!({
            "name": {"type": "string", "required": true, "description": "who"},
            "count": {"type": "integer"}
        }))
        .unwrap();
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "who"},
                    "count": {"type": "integer"}
                },
                "required": ["name"]
            })
        );
        let empty = parameter_schema_spec_to_json_schema(&json!({})).unwrap();
        assert_eq!(empty, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn parameter_required_must_be_true() {
        assert_eq!(
            parameter_schema_spec_to_json_schema(&json!({
                "a": {"type": "string", "required": false}
            }))
            .unwrap_err()
            .to_string(),
            "unsupported JSON schema: parameters.a.required must be true when present"
        );
    }

    #[test]
    fn validate_args_reports_argument_violations() {
        let spec = json!({
            "path": {"type": "string", "required": true},
            "lines": {"type": "array", "items": {"type": "integer"}}
        });
        let violations = validate_args(&spec, &json!({"lines": ["x"]})).unwrap();
        assert_eq!(
            violations,
            vec![
                "missing required property \"path\"".to_owned(),
                "\"lines[0]\" must be an integer".to_owned()
            ]
        );
        assert_eq!(
            validate_args(&spec, &json!({"path": "a", "lines": [1]})).unwrap(),
            [] as [std::string::String; 0]
        );
        let error = ToolArgsError::new(validate_args(&spec, &json!({})).unwrap());
        assert_eq!(
            error.to_string(),
            "invalid arguments: missing required property \"path\""
        );
    }

    #[test]
    fn compiles_deep_specs_without_recursion() {
        let depth = 20_000;
        let mut spec = json!({"type": "string"});
        for _ in 0..depth {
            let mut wrapper = Map::new();
            wrapper.insert("type".to_owned(), json!("array"));
            wrapper.insert("items".to_owned(), spec);
            spec = Value::Object(wrapper);
        }
        let schema = value_schema_spec_to_json_schema(&spec).unwrap();
        assert_eq!(
            validate_json_schema_value(&schema, &json!("x"), "value"),
            vec!["\"value\" must be an array".to_owned()]
        );
        std::mem::forget(schema);
        std::mem::forget(spec);
    }
}
