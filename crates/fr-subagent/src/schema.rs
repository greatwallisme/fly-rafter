// Rust guideline compliant 2026-11-06
//! The enforced JSON-Schema subset (dsh `core/tools/src/json-schema.ts`
//! `validateJsonSchemaValue` and `assertObjectJsonSchema` 1:1): the
//! node vocabulary is `type` (object/array/string/number/integer/
//! boolean/null), `properties`, `required`, `items`, `enum`, `const`,
//! `oneOf`, and `additionalProperties: false`; annotation-only schemas
//! accept any lossless JSON.
//!
//! The value walk mirrors dsh's explicit-frame machine with a bounded
//! explicit work queue (the same no-recursion contract; the queue caps
//! at the same depth the dsh frame stack would reach).

use serde_json::Value;

/// The maximum nested schema-node depth one validation walk enqueues
/// (stack-safety bound; schemas are pre-asserted, so real depths stay
/// far below).
const MAX_DEPTH: usize = 256;

/// One pending (schema, value, path) evaluation.
struct Frame<'a> {
    node: &'a Value,
    value: &'a Value,
    path: String,
    depth: usize,
}

/// Renders the root-aware diagnostic path (dsh `diagnosticPath`).
fn diagnostic_path(path: &str) -> &str {
    if path.is_empty() { "arguments" } else { path }
}

/// Appends one object property without a leading dot at an implicit root
/// (dsh `propertyPath`).
fn property_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// The scalar `enum`/`const` checks shared by every primitive arm (dsh
/// `checkScalarValue`).
fn scalar_violations(node: &Value, value: &Value, path: &str) -> Vec<String> {
    let mut violations = Vec::new();
    if let Some(allowed) = node.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|candidate| candidate == value) {
            violations.push(format!(
                "\"{}\" must be one of {}",
                diagnostic_path(path),
                serde_json::to_string(allowed).unwrap_or_default()
            ));
        }
    }
    if let Some(expected) = node.get("const") {
        if expected != value {
            violations.push(format!(
                "\"{}\" must be {}",
                diagnostic_path(path),
                serde_json::to_string(expected).unwrap_or_default()
            ));
        }
    }
    violations
}

/// Validates one value against the supported subset (dsh
/// `validateJsonSchemaValue`): the violation texts are verbatim.
#[must_use]
pub fn validate(schema: &Value, value: &Value) -> Vec<String> {
    let mut queue: Vec<Frame<'_>> = vec![Frame {
        node: schema,
        value,
        path: String::new(),
        depth: 0,
    }];
    let mut violations = Vec::new();
    while let Some(frame) = queue.pop() {
        if frame.depth > MAX_DEPTH {
            violations.push(format!(
                "\"{}\" exceeds the maximum schema depth",
                diagnostic_path(&frame.path)
            ));
            continue;
        }
        let node = frame.node;
        let value = frame.value;
        let path = frame.path;
        if let Some(one_of) = node.get("oneOf").and_then(Value::as_array) {
            let mut matches = 0_u64;
            for branch in one_of {
                if validate(branch, value).is_empty() {
                    matches += 1;
                }
            }
            if matches != 1 {
                violations.push(format!(
                    "\"{}\" must match exactly one oneOf branch (matched {matches})",
                    diagnostic_path(&path)
                ));
            }
            continue;
        }
        let Some(node_type) = node.get("type").and_then(Value::as_str) else {
            continue;
        };
        match node_type {
            "object" => {
                let Some(record) = value.as_object() else {
                    violations.push(format!("\"{}\" must be an object", diagnostic_path(&path)));
                    continue;
                };
                let properties = node.get("properties").and_then(Value::as_object);
                if let Some(required) = node.get("required").and_then(Value::as_array) {
                    for key in required {
                        let Some(key) = key.as_str() else {
                            continue;
                        };
                        if !record.contains_key(key) {
                            violations.push(format!(
                                "missing required property \"{}\"",
                                property_path(&path, key)
                            ));
                        }
                    }
                }
                if let Some(properties) = properties {
                    for (key, child) in properties {
                        if let Some(child_value) = record.get(key) {
                            queue.push(Frame {
                                node: child,
                                value: child_value,
                                path: property_path(&path, key),
                                depth: frame.depth + 1,
                            });
                        }
                    }
                }
                if node.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
                    for key in record.keys() {
                        if !properties.is_some_and(|props| props.contains_key(key)) {
                            violations.push(format!(
                                "\"{}\" is not a declared property (additionalProperties: false)",
                                property_path(&path, key)
                            ));
                        }
                    }
                }
            }
            "array" => {
                let Some(items) = value.as_array() else {
                    violations.push(format!("\"{}\" must be an array", diagnostic_path(&path)));
                    continue;
                };
                if let Some(item_schema) = node.get("items") {
                    for (index, entry) in items.iter().enumerate() {
                        queue.push(Frame {
                            node: item_schema,
                            value: entry,
                            path: format!("{path}[{index}]"),
                            depth: frame.depth + 1,
                        });
                    }
                }
            }
            "string" => {
                if value.is_string() {
                    violations.extend(scalar_violations(node, value, &path));
                } else {
                    violations.push(format!("\"{}\" must be a string", diagnostic_path(&path)));
                }
            }
            "number" => {
                if let Some(number) = value.as_f64() {
                    if number.is_finite() {
                        violations.extend(scalar_violations(node, value, &path));
                    } else {
                        violations.push(format!(
                            "\"{}\" must be a finite JSON number",
                            diagnostic_path(&path)
                        ));
                    }
                } else {
                    violations.push(format!("\"{}\" must be a number", diagnostic_path(&path)));
                }
            }
            "integer" => {
                if value.as_i64().is_some() || value.as_u64().is_some() {
                    violations.extend(scalar_violations(node, value, &path));
                } else {
                    violations.push(format!("\"{}\" must be an integer", diagnostic_path(&path)));
                }
            }
            "boolean" => {
                if value.is_boolean() {
                    violations.extend(scalar_violations(node, value, &path));
                } else {
                    violations.push(format!("\"{}\" must be a boolean", diagnostic_path(&path)));
                }
            }
            "null" => {
                if !value.is_null() {
                    violations.push(format!("\"{}\" must be null", diagnostic_path(&path)));
                }
            }
            _ => {
                // An unknown type keyword accepts any lossless JSON (the
                // dsh `catches` arm: the node cannot constrain).
            }
        }
    }
    violations
}

/// Asserts the enforced subset plus the object-root constraint (dsh
/// `assertObjectJsonSchema`): the structured-output schema is
/// object-rooted and uses only the supported node vocabulary.
///
/// # Errors
///
/// Returns the joined violation list.
pub fn assert_object_schema(schema: &Value) -> Result<(), String> {
    let mut violations = Vec::new();
    check_schema_node(schema, "schema", &mut violations, 0);
    if schema
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        != Some("object")
    {
        violations
            .push("schema.type must be \"object\" (structured output is object-rooted)".to_owned());
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations.join("; "))
    }
}

/// The schema-side subset check (dsh `checkSchemaNode`): every node must
/// be an object carrying only the supported vocabulary.
fn check_schema_node(node: &Value, path: &str, violations: &mut Vec<String>, depth: usize) {
    let Some(object) = node.as_object() else {
        violations.push(format!("{path} must be an object"));
        return;
    };
    if depth > MAX_DEPTH {
        violations.push(format!("{path} exceeds the maximum schema depth"));
        return;
    }
    let known = [
        "type",
        "properties",
        "required",
        "items",
        "enum",
        "const",
        "oneOf",
        "additionalProperties",
        "description",
        "title",
    ];
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            violations.push(format!("{path} has unsupported key \"{key}\""));
        }
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (key, child) in properties {
            check_schema_node(
                child,
                &format!("{path}.properties.{key}"),
                violations,
                depth + 1,
            );
        }
    }
    if let Some(items) = object.get("items") {
        check_schema_node(items, &format!("{path}.items"), violations, depth + 1);
    }
    if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
        for (index, branch) in one_of.iter().enumerate() {
            check_schema_node(
                branch,
                &format!("{path}.oneOf[{index}]"),
                violations,
                depth + 1,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{assert_object_schema, validate};
    use serde_json::json;

    #[test]
    fn the_object_subset_enforces_required_and_closed_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "count": { "type": "integer" },
            },
            "required": ["name"],
            "additionalProperties": false,
        });
        assert!(validate(&schema, &json!({ "name": "ok", "count": 2 })).is_empty());
        let violations = validate(&schema, &json!({ "count": "two", "extra": true }));
        assert!(
            violations
                .iter()
                .any(|text| text == "\"count\" must be an integer"),
            "{violations:?}"
        );
        assert!(
            violations.iter().any(|text| {
                text == "\"extra\" is not a declared property (additionalProperties: false)"
            }),
            "{violations:?}"
        );
        let violations = validate(&schema, &json!({}));
        assert_eq!(violations, ["missing required property \"name\""]);
    }

    #[test]
    fn the_scalar_arms_cover_enum_const_and_number_finiteness() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["fast", "safe"] },
                "level": { "type": "integer", "const": 3 },
                "ratio": { "type": "number" },
                "flag": { "type": "boolean" },
            },
        });
        let good = json!({
            "mode": "fast", "level": 3, "ratio": 0.5, "flag": false,
        });
        assert!(validate(&schema, &good).is_empty());
        let bad = json!({ "mode": "slow", "level": 4, "ratio": "half", "flag": 1 });
        let violations = validate(&schema, &bad);
        assert!(
            violations
                .iter()
                .any(|text| { text == "\"mode\" must be one of [\"fast\",\"safe\"]" })
        );
        assert!(
            violations
                .iter()
                .any(|text| { text == "\"level\" must be 3" })
        );
        assert!(
            violations
                .iter()
                .any(|text| text == "\"ratio\" must be a number")
        );
        assert!(
            violations
                .iter()
                .any(|text| text == "\"flag\" must be a boolean")
        );
    }

    #[test]
    fn one_of_requires_exactly_one_matching_branch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "pet": {
                    "oneOf": [
                        { "type": "string" },
                        { "type": "integer" },
                    ],
                },
            },
        });
        assert!(validate(&schema, &json!({ "pet": "cat" })).is_empty());
        let violations = validate(&schema, &json!({ "pet": true }));
        assert_eq!(
            violations,
            ["\"pet\" must match exactly one oneOf branch (matched 0)"]
        );
    }

    #[test]
    fn the_object_root_assertion_accepts_the_supported_subset_only() {
        let good = json!({
            "type": "object",
            "properties": { "answer": { "type": "integer" } },
            "required": ["answer"],
        });
        assert!(assert_object_schema(&good).is_ok());
        let array_root = json!({ "type": "array", "items": { "type": "string" } });
        let error = assert_object_schema(&array_root).expect_err("the array root refuses");
        assert!(
            error.contains("schema.type must be \"object\" (structured output is object-rooted)"),
            "{error}"
        );
        let unsupported = json!({
            "type": "object",
            "properties": { "x": { "type": "string", "pattern": "^a" } },
        });
        let error = assert_object_schema(&unsupported).expect_err("the unsupported key refuses");
        assert!(error.contains("unsupported key \"pattern\""), "{error}");
    }
}
