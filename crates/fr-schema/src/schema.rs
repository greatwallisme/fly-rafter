//! Declarative schema descriptors: validation and JSON Schema projection.
//!
//! The descriptor is the single declaration source (PRD 6.3): [`validate`]
//! and [`to_json_schema`] both read from one [`Schema`] value, so assembly
//! checks and dump documents can never drift apart.

use serde_json::{Map, Value};

/// A plugin config schema declaration.
///
/// The M1 face covers the value shapes the assembly defaults require
/// (PRD 6.4). Declarations outside the face cannot be constructed, so an
/// unsupported declaration fails at compile time rather than at validation
/// time (fail-closed). Defaults are typed per variant, so a default that
/// violates its own declaration is likewise unconstructible.
#[derive(Debug, Clone, PartialEq)]
pub enum Schema {
    /// UTF-8 string; a non-empty `candidates` list restricts to enumeration
    /// membership.
    String {
        /// Allowed values; empty means any string.
        candidates: Vec<String>,
        /// Declared default, projected to the `default` annotation.
        default: Option<String>,
        /// Projected to the `description` annotation.
        description: Option<String>,
    },
    /// JSON integer (no fractional part).
    Integer {
        /// Inclusive lower bound.
        min: Option<i64>,
        /// Inclusive upper bound.
        max: Option<i64>,
        /// Declared default, projected to the `default` annotation.
        default: Option<i64>,
        /// Projected to the `description` annotation.
        description: Option<String>,
    },
    /// JSON number (integers included, per JSON Schema semantics).
    Number {
        /// Inclusive lower bound.
        min: Option<f64>,
        /// Inclusive upper bound.
        max: Option<f64>,
        /// Declared default, projected to the `default` annotation.
        default: Option<f64>,
        /// Projected to the `description` annotation.
        description: Option<String>,
    },
    /// Boolean.
    Boolean {
        /// Declared default, projected to the `default` annotation.
        default: Option<bool>,
        /// Projected to the `description` annotation.
        description: Option<String>,
    },
    /// Object with declared properties. `additional_properties` is explicit;
    /// `false` rejects unknown keys (fail-closed, mirroring the tool schema
    /// discipline of REQ-FR-TOOLS-5).
    Object {
        /// Declared keys in declaration order.
        properties: Vec<Property>,
        /// Whether undeclared keys are allowed.
        additional_properties: bool,
        /// Projected to the `description` annotation.
        description: Option<String>,
    },
    /// Array whose elements all share one schema.
    Array {
        /// Element schema.
        items: Box<Self>,
        /// Projected to the `description` annotation.
        description: Option<String>,
    },
    /// String-keyed map whose values share one schema (the dsh dict form).
    Dict {
        /// Value schema applied to every key.
        values: Box<Self>,
        /// Projected to the `description` annotation.
        description: Option<String>,
    },
}

/// One declared object key.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// Object key.
    pub name: String,
    /// Whether the key must be present.
    pub required: bool,
    /// Value schema.
    pub schema: Schema,
}

/// One validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Dot path to the offending value: object keys join with `.` and array
    /// elements append `[i]`; empty for the root value.
    pub path: String,
    /// Human-readable violation description.
    pub message: String,
}

/// Validates `value` against `schema`, returning every violation (empty when
/// the value conforms).
#[must_use]
pub fn validate(schema: &Schema, value: &Value) -> Vec<Violation> {
    let mut violations = Vec::new();
    validate_value(schema, value, "", &mut violations);
    violations
}

fn validate_value(schema: &Schema, value: &Value, path: &str, out: &mut Vec<Violation>) {
    match schema {
        Schema::String { candidates, .. } => {
            let Some(text) = value.as_str() else {
                push(out, path, "expected string");
                return;
            };
            if !candidates.is_empty() && !candidates.iter().any(|candidate| candidate == text) {
                push(out, path, "value not in candidates");
            }
        }
        Schema::Integer { min, max, .. } => {
            // JSON Schema 2020-12 counts any zero-fraction numeric value as
            // an integer (`1.0` included), so acceptance runs in the f64
            // domain like the dsh runtime; the declared bounds widen
            // accordingly instead of rejecting integral floats the projected
            // schema would accept.
            let Some(number) = value.as_f64() else {
                push(out, path, "expected integer");
                return;
            };
            if number.fract() != 0.0 {
                push(out, path, "expected integer");
                return;
            }
            check_f64_range(out, path, number, (*min).map(widen), (*max).map(widen));
        }
        Schema::Number { min, max, .. } => {
            let Some(number) = value.as_f64() else {
                push(out, path, "expected number");
                return;
            };
            check_f64_range(out, path, number, *min, *max);
        }
        Schema::Boolean { .. } => {
            if !value.is_boolean() {
                push(out, path, "expected boolean");
            }
        }
        Schema::Object {
            properties,
            additional_properties,
            ..
        } => {
            let Some(map) = value.as_object() else {
                push(out, path, "expected object");
                return;
            };
            for property in properties {
                match map.get(&property.name) {
                    Some(property_value) => validate_value(
                        &property.schema,
                        property_value,
                        &join(path, &property.name),
                        out,
                    ),
                    None if property.required => {
                        push(
                            out,
                            path,
                            format!("missing required key `{}`", property.name),
                        );
                    }
                    None => {}
                }
            }
            if !additional_properties {
                for key in map.keys() {
                    if !properties.iter().any(|property| property.name == *key) {
                        push(out, path, format!("unknown key `{key}`"));
                    }
                }
            }
        }
        Schema::Array { items, .. } => {
            let Some(array) = value.as_array() else {
                push(out, path, "expected array");
                return;
            };
            for (index, element) in array.iter().enumerate() {
                validate_value(items, element, &format!("{path}[{index}]"), out);
            }
        }
        Schema::Dict { values, .. } => {
            let Some(map) = value.as_object() else {
                push(out, path, "expected object");
                return;
            };
            for (key, dict_value) in map {
                validate_value(values, dict_value, &join(path, key), out);
            }
        }
    }
}

/// Widens an inclusive `i64` bound to the f64 validation domain.
///
/// The precision loss at the `i64` extremes is intentional: the dsh runtime
/// evaluates every number as a double, so the widened bound classifies
/// borderline values exactly as the mirrored behavior does.
#[expect(
    clippy::cast_precision_loss,
    reason = "validation domain mirrors the dsh double runtime"
)]
const fn widen(bound: i64) -> f64 {
    bound as f64
}

fn check_f64_range(
    out: &mut Vec<Violation>,
    path: &str,
    number: f64,
    min: Option<f64>,
    max: Option<f64>,
) {
    if let Some(min) = min
        && number < min
    {
        push(out, path, "below minimum");
    }
    if let Some(max) = max
        && number > max
    {
        push(out, path, "above maximum");
    }
}

fn push(out: &mut Vec<Violation>, path: &str, message: impl Into<String>) {
    out.push(Violation {
        path: path.to_string(),
        message: message.into(),
    });
}

/// Joins a parent path with a child key (`""` roots the key).
fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Projects the declaration to a JSON Schema 2020-12 document.
///
/// Keyword face aligned with dsh `ConfigJsonSchemaObject`
/// (config-schema/types.ts:7-26), carried through the dsh projector faces:
/// per-node null acceptance (projector.ts:90-97, 426) and loader-expression
/// wrapping at every value position (projector.ts:100-122, 447).
///
/// The root declaration carries no requiredness (the dsh default for an
/// undeclared `meta.required`), so the projected root accepts null; nested
/// nodes take each [`Property::required`] flag. Annotating keywords hoist to
/// the front of each wrapped position, matching the dsh dump's `configN`
/// shape.
#[must_use]
pub fn to_json_schema(schema: &Schema) -> Value {
    wrap_value_positions(project_core(schema, false))
}

/// Projects one node structurally, then applies the node's null acceptance
/// (dsh `nullable(core, acceptsMissing !== false)`); children project with
/// their own requiredness.
fn project_core(schema: &Schema, required: bool) -> Value {
    let (mut object, type_name) = match schema {
        Schema::String {
            candidates,
            default,
            description,
        } => {
            let mut object = type_object("string");
            if !candidates.is_empty() {
                object.insert("enum".into(), Value::from(candidates.clone()));
            }
            insert_annotations(
                &mut object,
                default.clone().map(Value::String),
                description.as_deref(),
            );
            (object, "string")
        }
        Schema::Integer {
            min,
            max,
            default,
            description,
        } => {
            let mut object = type_object("integer");
            insert_i64_bounds(&mut object, *min, *max);
            insert_annotations(
                &mut object,
                (*default).map(Value::from),
                description.as_deref(),
            );
            (object, "integer")
        }
        Schema::Number {
            min,
            max,
            default,
            description,
        } => {
            let mut object = type_object("number");
            insert_f64_bounds(&mut object, *min, *max);
            insert_annotations(
                &mut object,
                (*default).map(Value::from),
                description.as_deref(),
            );
            (object, "number")
        }
        Schema::Boolean {
            default,
            description,
        } => {
            let mut object = type_object("boolean");
            insert_annotations(
                &mut object,
                (*default).map(Value::from),
                description.as_deref(),
            );
            (object, "boolean")
        }
        Schema::Object {
            properties,
            additional_properties,
            description,
        } => {
            let (property_map, required) = project_properties(properties);
            let mut object = type_object("object");
            object.insert("properties".into(), Value::Object(property_map));
            if !required.is_empty() {
                object.insert("required".into(), Value::from(required));
            }
            object.insert(
                "additionalProperties".into(),
                Value::from(*additional_properties),
            );
            insert_annotations(&mut object, None, description.as_deref());
            (object, "object")
        }
        Schema::Array { items, description } => {
            let mut object = type_object("array");
            object.insert("items".into(), project_core(items, false));
            insert_annotations(&mut object, None, description.as_deref());
            (object, "array")
        }
        Schema::Dict {
            values,
            description,
        } => {
            let mut object = type_object("object");
            object.insert("additionalProperties".into(), project_core(values, false));
            insert_annotations(&mut object, None, description.as_deref());
            (object, "object")
        }
    };
    nullable(&mut object, type_name, !required);
    Value::Object(object)
}

/// Adds or removes null acceptance on one projected node (dsh
/// `projector.ts:90-97`): an accepted node merges `null` into its `type`
/// list, a required node appends `not: {"type": "null"}`. The node's type
/// name passes in from the constructing arm, so the function is total; dsh's
/// typeless-`anyOf` and boolean-core forms cannot occur here (fr's restricted
/// face always projects a string `type`, PRD 6.3 subset, fail-closed).
fn nullable(core: &mut Map<String, Value>, type_name: &str, accepts: bool) {
    if accepts {
        // dsh dedups through a Set; fr's projected type is always a single
        // string, so the merge is a fixed two-entry list.
        core.insert(
            "type".into(),
            Value::Array(vec![Value::from(type_name), Value::from("null")]),
        );
    } else {
        let mut null_type = Map::new();
        null_type.insert("type".into(), Value::from("null"));
        core.insert("not".into(), Value::Object(null_type));
    }
}

/// Wraps every config value position with the loader-expression alternative
/// (dsh `expressions`, projector.ts:100-122): `properties` values, `items`,
/// and `additionalProperties` subtrees wrap recursively, the position's
/// annotating keywords hoist to the front in dsh's list order, and the
/// position becomes `{...annotations, anyOf: [core, loaderExpression]}`.
/// The dsh branches for `propertyNames`, `prefixItems`, `anyOf`, and `not`
/// are unreachable here: fr's restricted projection face never emits those
/// keywords, so such subtrees cannot occur (PRD 6.3 subset).
fn wrap_value_positions(value: Value) -> Value {
    let Value::Object(mut object) = value else {
        return value; // non-object positions (`additionalProperties: false`) stay verbatim
    };
    for key in ["properties", "items", "additionalProperties"] {
        let Some(child) = object.shift_remove(key) else {
            continue;
        };
        let walked = match child {
            // Property positions wrap individually; the `properties` map
            // itself is not a value position (dsh valuePosition=false there).
            Value::Object(children) if key == "properties" => Value::Object(
                children
                    .into_iter()
                    .map(|(name, child)| (name, wrap_value_positions(child)))
                    .collect(),
            ),
            child => wrap_value_positions(child),
        };
        object.insert(key.into(), walked);
    }
    let mut wrapped = Map::new();
    for key in ["title", "description", "default", "$comment", "x-fr"] {
        if let Some(annotation) = object.shift_remove(key) {
            wrapped.insert(key.into(), annotation);
        }
    }
    let mut reference = Map::new();
    reference.insert("$ref".into(), Value::from("#/$defs/loaderExpression"));
    wrapped.insert(
        "anyOf".into(),
        Value::Array(vec![Value::Object(object), Value::Object(reference)]),
    );
    Value::Object(wrapped)
}

/// Creates the `{"type": <name>}` skeleton every projection shares.
fn type_object(type_name: &str) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("type".into(), Value::from(type_name));
    object
}

/// Projects declared properties and the `required` list in declaration order.
fn project_properties(properties: &[Property]) -> (Map<String, Value>, Vec<Value>) {
    let mut property_map = Map::new();
    let mut required = Vec::new();
    for property in properties {
        property_map.insert(
            property.name.clone(),
            project_core(&property.schema, property.required),
        );
        if property.required {
            required.push(Value::from(property.name.clone()));
        }
    }
    (property_map, required)
}

fn insert_i64_bounds(object: &mut Map<String, Value>, min: Option<i64>, max: Option<i64>) {
    if let Some(min) = min {
        object.insert("minimum".into(), Value::from(min));
    }
    if let Some(max) = max {
        object.insert("maximum".into(), Value::from(max));
    }
}

fn insert_f64_bounds(object: &mut Map<String, Value>, min: Option<f64>, max: Option<f64>) {
    if let Some(min) = min {
        object.insert("minimum".into(), Value::from(min));
    }
    if let Some(max) = max {
        object.insert("maximum".into(), Value::from(max));
    }
}

fn insert_annotations(
    object: &mut Map<String, Value>,
    default: Option<Value>,
    description: Option<&str>,
) {
    if let Some(default) = default {
        object.insert("default".into(), default);
    }
    if let Some(description) = description {
        object.insert("description".into(), Value::from(description));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_rejects_type_mismatch_and_honors_candidates() {
        let schema = Schema::String {
            candidates: vec!["ask".into(), "never".into()],
            default: None,
            description: None,
        };
        assert_eq!(validate(&schema, &json!("ask")), Vec::new());
        let violations = validate(&schema, &json!("always"));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].message, "value not in candidates");
        let violations = validate(&schema, &json!(1));
        assert_eq!(violations[0].message, "expected string");
    }

    #[test]
    fn string_without_candidates_accepts_any_string() {
        let schema = Schema::String {
            candidates: Vec::new(),
            default: None,
            description: None,
        };
        assert_eq!(validate(&schema, &json!("anything")), Vec::new());
    }

    #[test]
    fn integer_bounds_reject_out_of_range() {
        let schema = Schema::Integer {
            min: Some(1),
            max: Some(10),
            default: None,
            description: None,
        };
        assert_eq!(validate(&schema, &json!(5)), Vec::new());
        assert_eq!(validate(&schema, &json!(0))[0].message, "below minimum");
        assert_eq!(validate(&schema, &json!(11))[0].message, "above maximum");
        assert_eq!(
            validate(&schema, &json!(1.5))[0].message,
            "expected integer"
        );
    }

    #[test]
    fn integer_matches_json_schema_zero_fraction_semantics() {
        let schema = Schema::Integer {
            min: None,
            max: None,
            default: None,
            description: None,
        };
        assert_eq!(validate(&schema, &json!(1.0)), Vec::new());
        assert_eq!(
            validate(&schema, &json!(9_223_372_036_854_775_808_u64)),
            Vec::new()
        );
        let violations = validate(&schema, &json!(1.5));
        assert_eq!(violations[0].message, "expected integer");
    }

    #[test]
    fn number_accepts_integers_and_rejects_non_numeric() {
        let schema = Schema::Number {
            min: None,
            max: None,
            default: None,
            description: None,
        };
        assert_eq!(validate(&schema, &json!(3)), Vec::new());
        assert_eq!(validate(&schema, &json!(2.5)), Vec::new());
        assert_eq!(validate(&schema, &json!("3"))[0].message, "expected number");
    }

    #[test]
    fn boolean_rejects_numeric_one() {
        let schema = Schema::Boolean {
            default: None,
            description: None,
        };
        assert_eq!(validate(&schema, &json!(true)), Vec::new());
        assert_eq!(validate(&schema, &json!(1))[0].message, "expected boolean");
    }

    #[test]
    fn object_rejects_unknown_keys_when_additional_properties_false() {
        let schema = Schema::Object {
            properties: vec![Property {
                name: "model".into(),
                required: false,
                schema: string_schema(),
            }],
            additional_properties: false,
            description: None,
        };
        assert_eq!(
            validate(&schema, &json!({ "model": "deepseek-flash" })),
            Vec::new()
        );
        let violations = validate(&schema, &json!({ "model": "deepseek-flash", "extra": 1 }));
        assert_eq!(violations[0].message, "unknown key `extra`");
    }

    #[test]
    fn object_unknown_keys_allowed_when_additional_properties_true() {
        let schema = Schema::Object {
            properties: Vec::new(),
            additional_properties: true,
            description: None,
        };
        assert_eq!(
            validate(&schema, &json!({ "anything": [1, 2] })),
            Vec::new()
        );
    }

    #[test]
    fn object_requires_declared_keys() {
        let schema = Schema::Object {
            properties: vec![Property {
                name: "model".into(),
                required: true,
                schema: string_schema(),
            }],
            additional_properties: false,
            description: None,
        };
        let violations = validate(&schema, &json!({}));
        assert_eq!(violations[0].message, "missing required key `model`");
        assert_eq!(validate(&schema, &json!({ "model": "x" })), Vec::new());
    }

    #[test]
    fn nested_violation_paths_use_dot_and_bracket_format() {
        let schema = Schema::Object {
            properties: vec![Property {
                name: "outer".into(),
                required: false,
                schema: Schema::Object {
                    properties: vec![Property {
                        name: "inner".into(),
                        required: false,
                        schema: Schema::Array {
                            items: Box::new(string_schema()),
                            description: None,
                        },
                    }],
                    additional_properties: false,
                    description: None,
                },
            }],
            additional_properties: false,
            description: None,
        };
        let violations = validate(&schema, &json!({ "outer": { "inner": ["ok", 7] } }));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, "outer.inner[1]");
        assert_eq!(violations[0].message, "expected string");
    }

    #[test]
    fn dict_validates_every_value() {
        let schema = Schema::Dict {
            values: Box::new(Schema::Integer {
                min: Some(0),
                max: None,
                default: None,
                description: None,
            }),
            description: None,
        };
        assert_eq!(validate(&schema, &json!({ "a": 1, "b": 0 })), Vec::new());
        let violations = validate(&schema, &json!({ "a": 1, "b": -1 }));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, "b");
        assert_eq!(violations[0].message, "below minimum");
    }

    #[test]
    fn projection_string_with_enum_and_annotations() {
        let schema = Schema::String {
            candidates: vec!["ask".into(), "never".into()],
            default: Some("ask".to_string()),
            description: Some("approval policy".into()),
        };
        // The root position is not required (dsh's undeclared `meta.required`
        // default), so `null` merges into the type list; annotations hoist to
        // the front and the loader-expression alternative closes the `anyOf`.
        assert_eq!(
            to_json_schema(&schema),
            json!({
                "description": "approval policy",
                "default": "ask",
                "anyOf": [
                    { "type": ["string", "null"], "enum": ["ask", "never"] },
                    { "$ref": "#/$defs/loaderExpression" },
                ],
            })
        );
    }

    #[test]
    fn projection_integer_and_number_bounds() {
        let integer = Schema::Integer {
            min: Some(0),
            max: Some(600_000),
            default: Some(60_000),
            description: None,
        };
        assert_eq!(
            to_json_schema(&integer),
            json!({
                "default": 60_000,
                "anyOf": [
                    { "type": ["integer", "null"], "minimum": 0, "maximum": 600_000 },
                    { "$ref": "#/$defs/loaderExpression" },
                ],
            })
        );
        let number = Schema::Number {
            min: Some(0.0),
            max: None,
            default: None,
            description: None,
        };
        assert_eq!(
            to_json_schema(&number),
            json!({
                "anyOf": [
                    { "type": ["number", "null"], "minimum": 0.0 },
                    { "$ref": "#/$defs/loaderExpression" },
                ],
            })
        );
    }

    #[test]
    fn projection_object_array_and_dict() {
        let object = Schema::Object {
            properties: vec![Property {
                name: "model".into(),
                required: true,
                schema: string_schema(),
            }],
            additional_properties: false,
            description: None,
        };
        assert_eq!(
            to_json_schema(&object),
            json!({
                "anyOf": [
                    {
                        "type": ["object", "null"],
                        "properties": {
                            "model": {
                                "anyOf": [
                                    { "type": "string", "not": { "type": "null" } },
                                    { "$ref": "#/$defs/loaderExpression" },
                                ],
                            },
                        },
                        "required": ["model"],
                        "additionalProperties": false,
                    },
                    { "$ref": "#/$defs/loaderExpression" },
                ],
            })
        );
        let array = Schema::Array {
            items: Box::new(Schema::Integer {
                min: None,
                max: None,
                default: None,
                description: None,
            }),
            description: None,
        };
        assert_eq!(
            to_json_schema(&array),
            json!({
                "anyOf": [
                    {
                        "type": ["array", "null"],
                        "items": {
                            "anyOf": [
                                { "type": ["integer", "null"] },
                                { "$ref": "#/$defs/loaderExpression" },
                            ],
                        },
                    },
                    { "$ref": "#/$defs/loaderExpression" },
                ],
            })
        );
        let dict = Schema::Dict {
            values: Box::new(string_schema()),
            description: None,
        };
        assert_eq!(
            to_json_schema(&dict),
            json!({
                "anyOf": [
                    {
                        "type": ["object", "null"],
                        "additionalProperties": {
                            "anyOf": [
                                { "type": ["string", "null"] },
                                { "$ref": "#/$defs/loaderExpression" },
                            ],
                        },
                    },
                    { "$ref": "#/$defs/loaderExpression" },
                ],
            })
        );
    }

    #[test]
    fn validation_and_projection_share_one_declaration() {
        let schema = Schema::Object {
            properties: vec![
                Property {
                    name: "model".into(),
                    required: true,
                    schema: Schema::String {
                        candidates: vec!["deepseek-flash".into()],
                        default: Some("deepseek-flash".to_string()),
                        description: None,
                    },
                },
                Property {
                    name: "thresholds".into(),
                    required: false,
                    schema: Schema::Array {
                        items: Box::new(Schema::Integer {
                            min: None,
                            max: None,
                            default: None,
                            description: None,
                        }),
                        description: None,
                    },
                },
            ],
            additional_properties: false,
            description: None,
        };
        assert_eq!(
            validate(&schema, &json!({ "model": "deepseek-flash" })),
            Vec::new()
        );
        let violations = validate(&schema, &json!({ "model": "other", "thresholds": ["x"] }));
        assert_eq!(violations.len(), 2);
        assert_eq!(
            to_json_schema(&schema),
            json!({
                "anyOf": [
                    {
                        "type": ["object", "null"],
                        "properties": {
                            "model": {
                                "default": "deepseek-flash",
                                "anyOf": [
                                    { "type": "string", "enum": ["deepseek-flash"], "not": { "type": "null" } },
                                    { "$ref": "#/$defs/loaderExpression" },
                                ],
                            },
                            "thresholds": {
                                "anyOf": [
                                    {
                                        "type": ["array", "null"],
                                        "items": {
                                            "anyOf": [
                                                { "type": ["integer", "null"] },
                                                { "$ref": "#/$defs/loaderExpression" },
                                            ],
                                        },
                                    },
                                    { "$ref": "#/$defs/loaderExpression" },
                                ],
                            },
                        },
                        "required": ["model"],
                        "additionalProperties": false,
                    },
                    { "$ref": "#/$defs/loaderExpression" },
                ],
            })
        );
    }

    fn string_schema() -> Schema {
        Schema::String {
            candidates: Vec::new(),
            default: None,
            description: None,
        }
    }
}
