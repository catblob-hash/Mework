//! Closed JSON Schema subset for structured subagent output.
//!
//! This module depends only on `serde_json`. It supports a closed set of
//! constraint keywords: `type`, `properties`, `required`, `items`, `enum`,
//! `const`, `additionalProperties`, `minItems`/`maxItems`,
//! `minLength`/`maxLength`, and `minimum`/`maximum`. A separate closed set of
//! annotation-only keywords (`title`/`description`/`default`/`examples`/
//! `$schema`/`$comment`) is accepted and ignored because it expresses no
//! constraint.
//!
//! All other keywords are rejected at compile time rather than silently ignored.
//! A silently ignored constraint would falsely tell callers that output is
//! constrained; reporting it as an `agent_spawn` argument error lets the model
//! correct the unsupported schema immediately.
//!
//! Both traversals are iterative and use no recursion. Their depth limit is
//! 10,000, before which recursive descent would exhaust the thread stack.

use std::collections::VecDeque;

use serde_json::{Map, Value};

/// Maximum JSON node count in a schema document.
pub const MAX_SCHEMA_NODES: usize = 100_000;
/// Maximum schema nesting depth.
///
/// Model-provided JSON cannot reach this limit because `serde_json` rejects
/// nesting beyond 128 levels while parsing. The limit protects host-constructed
/// schemas, such as workflow scripts that programmatically build a deep `Value`.
/// Since `Value::Drop` and `Clone` recurse natively, iterative traversals reject
/// excessive depth before those operations can exhaust the stack.
pub const MAX_SCHEMA_DEPTH: usize = 10_000;
/// Maximum errors collected in one validation. Collection stops after this cap
/// and appends a summary error.
pub const MAX_VALIDATION_ERRORS: usize = 100;

/// Supported constraint keywords, ordered as they appear in error messages.
pub const SUPPORTED_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "items",
    "enum",
    "const",
    "additionalProperties",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
];

/// Annotation-only keywords: accepted and ignored because they carry no constraint.
pub const ANNOTATION_KEYWORDS: &[&str] = &[
    "$schema",
    "$comment",
    "$id",
    "title",
    "description",
    "default",
    "examples",
];

/// Permitted `type` values.
pub const SUPPORTED_TYPES: &[&str] = &[
    "object", "array", "string", "number", "integer", "boolean", "null",
];

/// A schema document that passed structural prevalidation.
///
/// [`compile`] is the only constructor, so holding a `Schema` proves its shape
/// is valid and lets [`Schema::validate`] assume a well-formed schema.
#[derive(Clone, Debug, PartialEq)]
pub struct Schema {
    root: Value,
}

impl Schema {
    /// The original schema document, ready for use as a tool descriptor's
    /// `input_schema`.
    pub fn as_value(&self) -> &Value {
        &self.root
    }

    pub fn into_value(self) -> Value {
        self.root
    }

    /// Validate an instance and return all errors with instance paths rather
    /// than only the first error.
    pub fn validate(&self, instance: &Value) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let mut truncated = false;
        let mut queue: VecDeque<(&Value, &Value, String)> = VecDeque::new();
        queue.push_back((&self.root, instance, String::new()));
        while let Some((schema, value, path)) = queue.pop_front() {
            let Some(map) = schema.as_object() else {
                continue;
            };
            validate_node(map, value, &path, &mut errors, &mut queue);
            // Checked AFTER the node, not only before it: one object node emits
            // an error per unexpected key and per missing required name, so a
            // budget consulted only between pops bounds QUEUE ENTRIES rather
            // than errors, and a wide object blows past it inside one call.
            if errors.len() >= MAX_VALIDATION_ERRORS {
                truncated = true;
                break;
            }
        }
        if truncated {
            // Reports what is actually kept. `validate_node` finishes the node
            // it is on before the budget is re-checked, so the list can overrun
            // the cap — announcing the constant without trimming to it would be
            // a false count, and the whole list is returned verbatim to the
            // child as tool-result text.
            errors.truncate(MAX_VALIDATION_ERRORS);
            errors.push(format!(
                "Too many errors; only the first {MAX_VALIDATION_ERRORS} are reported. Correct the issues above and try again"
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Render [`Schema::validate`] errors into a message that can be returned
    /// directly to the model.
    pub fn validation_message(errors: &[String]) -> String {
        let mut message = String::from("Structured output does not conform to output_schema:");
        for error in errors {
            message.push_str("\n- ");
            message.push_str(error);
        }
        message
    }
}

/// Prevalidate schema size, then each keyword and its value shape.
///
/// Success guarantees [`Schema::validate`] cannot yield misleading results due
/// to a malformed schema.
pub fn compile(schema: &Value) -> Result<Schema, String> {
    check_bounds(schema)?;
    let root = schema
        .as_object()
        .ok_or_else(|| "output_schema must be a JSON object".to_string())?;
    // Structured output returns through the `structured_output` tool, whose
    // input is an object for every provider, so the root schema must be object.
    match root.get("type") {
        Some(Value::String(name)) if name == "object" => {}
        Some(_) => {
            return Err("output_schema root type must be \"object\"".to_string());
        }
        None => {
            return Err("output_schema root is missing type; it must be \"object\"".to_string());
        }
    }
    check_schema_shape(schema)?;
    Ok(Schema {
        root: schema.clone(),
    })
}

/// Iteratively count nodes and depth, rejecting either overage with its own error.
fn check_bounds(root: &Value) -> Result<(), String> {
    let mut stack: Vec<(&Value, usize)> = vec![(root, 1)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_SCHEMA_NODES {
            return Err(format!(
                "output_schema is too large: node count exceeds {MAX_SCHEMA_NODES}"
            ));
        }
        if depth > MAX_SCHEMA_DEPTH {
            return Err(format!(
                "output_schema is too deep: nesting exceeds {MAX_SCHEMA_DEPTH}"
            ));
        }
        match value {
            Value::Object(map) => {
                for entry in map.values() {
                    stack.push((entry, depth + 1));
                }
            }
            Value::Array(items) => {
                for entry in items {
                    stack.push((entry, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Iteratively validate every subschema's keyword set and value shapes.
fn check_schema_shape(root: &Value) -> Result<(), String> {
    let mut stack: Vec<(&Value, String)> = vec![(root, String::new())];
    while let Some((value, path)) = stack.pop() {
        let map = value
            .as_object()
            .ok_or_else(|| format!("{} must be a JSON object", schema_location(&path)))?;
        for (key, entry) in map {
            if ANNOTATION_KEYWORDS.contains(&key.as_str()) {
                continue;
            }
            if !SUPPORTED_KEYWORDS.contains(&key.as_str()) {
                return Err(format!(
                    "{} uses unsupported keyword {key}; supported keywords are {}",
                    schema_location(&path),
                    SUPPORTED_KEYWORDS.join(", ")
                ));
            }
            check_keyword(key, entry, &path, &mut stack)?;
        }
        check_cross_keyword(map, &path)?;
    }
    Ok(())
}

fn check_keyword<'a>(
    key: &str,
    entry: &'a Value,
    path: &str,
    stack: &mut Vec<(&'a Value, String)>,
) -> Result<(), String> {
    let here = schema_location(path);
    match key {
        "type" => match entry {
            Value::String(name) => {
                if !SUPPORTED_TYPES.contains(&name.as_str()) {
                    return Err(format!(
                        "{here} has unsupported type value {name}; allowed values are {}",
                        SUPPORTED_TYPES.join(", ")
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "{here} type must be a string; allowed values are {}",
                    SUPPORTED_TYPES.join(", ")
                ));
            }
        },
        "properties" => {
            let props = entry
                .as_object()
                .ok_or_else(|| format!("{here} properties must be an object"))?;
            for (name, sub) in props {
                stack.push((sub, format!("{path}/properties/{name}")));
            }
        }
        "required" => {
            let names = entry
                .as_array()
                .ok_or_else(|| format!("{here} required must be an array of strings"))?;
            let mut seen: Vec<&str> = Vec::with_capacity(names.len());
            for name in names {
                let name = name
                    .as_str()
                    .ok_or_else(|| format!("{here} required may contain only strings"))?;
                if seen.contains(&name) {
                    return Err(format!("{here} required contains duplicate entry {name}"));
                }
                seen.push(name);
            }
        }
        "items" => {
            if entry.is_array() {
                return Err(format!(
                    "{here} items supports one subschema only, not tuple-form arrays"
                ));
            }
            stack.push((entry, format!("{path}/items")));
        }
        "enum" => {
            let values = entry
                .as_array()
                .ok_or_else(|| format!("{here} enum must be an array"))?;
            if values.is_empty() {
                return Err(format!("{here} enum must not be empty"));
            }
        }
        "const" => {}
        "additionalProperties" => match entry {
            Value::Bool(_) => {}
            Value::Object(_) => {
                stack.push((entry, format!("{path}/additionalProperties")));
            }
            _ => {
                return Err(format!(
                    "{here} additionalProperties must be a Boolean or subschema"
                ));
            }
        },
        "minItems" | "maxItems" | "minLength" | "maxLength" => {
            if entry.as_u64().is_none() {
                return Err(format!("{here} {key} must be a non-negative integer"));
            }
        }
        "minimum" | "maximum" => {
            if entry.as_f64().is_none() {
                return Err(format!("{here} {key} must be a number"));
            }
        }
        // `check_schema_shape` already filtered with SUPPORTED_KEYWORDS.
        other => return Err(format!("{here} uses unsupported keyword {other}")),
    }
    Ok(())
}

/// Check consistency among keywords at one level, including empty ranges and
/// unsatisfiable required properties.
fn check_cross_keyword(map: &Map<String, Value>, path: &str) -> Result<(), String> {
    let here = schema_location(path);
    if let (Some(low), Some(high)) = (
        map.get("minItems").and_then(Value::as_u64),
        map.get("maxItems").and_then(Value::as_u64),
    ) {
        if low > high {
            return Err(format!("{here} minItems {low} is greater than maxItems {high}"));
        }
    }
    if let (Some(low), Some(high)) = (
        map.get("minLength").and_then(Value::as_u64),
        map.get("maxLength").and_then(Value::as_u64),
    ) {
        if low > high {
            return Err(format!("{here} minLength {low} is greater than maxLength {high}"));
        }
    }
    if let (Some(low), Some(high)) = (
        map.get("minimum").and_then(Value::as_f64),
        map.get("maximum").and_then(Value::as_f64),
    ) {
        if low > high {
            return Err(format!("{here} minimum {low} is greater than maximum {high}"));
        }
    }
    // With `additionalProperties: false`, `properties` is exhaustive, so a
    // required name absent from it is unsatisfiable and causes endless retries.
    if map.get("additionalProperties") == Some(&Value::Bool(false)) {
        let declared = map.get("properties").and_then(Value::as_object);
        if let Some(names) = map.get("required").and_then(Value::as_array) {
            for name in names {
                let Some(name) = name.as_str() else { continue };
                let known = declared.is_some_and(|props| props.contains_key(name));
                if !known {
                    return Err(format!(
                        "{here} required includes {name}, but additionalProperties is false and properties does not declare it, so the schema cannot be satisfied"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_node<'a>(
    schema: &'a Map<String, Value>,
    value: &'a Value,
    path: &str,
    errors: &mut Vec<String>,
    queue: &mut VecDeque<(&'a Value, &'a Value, String)>,
) {
    let here = instance_location(path);
    if let Some(Value::String(expected)) = schema.get("type") {
        if !type_matches(expected, value) {
            errors.push(format!(
                "{here} expected {expected}, but got {}",
                type_name(value)
            ));
            // Further range checks would only add derived noise after a type mismatch.
            return;
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|candidate| json_equal(candidate, value)) {
            errors.push(format!("{here} is not one of the values allowed by enum"));
        }
    }
    if let Some(expected) = schema.get("const") {
        if !json_equal(expected, value) {
            errors.push(format!("{here} must equal the value specified by const"));
        }
    }
    match value {
        Value::Object(instance) => {
            let properties = schema.get("properties").and_then(Value::as_object);
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for name in required {
                    let Some(name) = name.as_str() else { continue };
                    if !instance.contains_key(name) {
                        errors.push(format!("{here} is missing required field {name}"));
                    }
                }
            }
            let additional = schema.get("additionalProperties");
            for (name, child) in instance {
                if let Some(sub) = properties.and_then(|props| props.get(name)) {
                    queue.push_back((sub, child, format!("{path}/{name}")));
                    continue;
                }
                match additional {
                    Some(Value::Bool(false)) => errors.push(format!(
                        "{here} contains undeclared field {name}, but additionalProperties is false"
                    )),
                    Some(sub @ Value::Object(_)) => {
                        queue.push_back((sub, child, format!("{path}/{name}")));
                    }
                    _ => {}
                }
            }
        }
        Value::Array(items) => {
            if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
                if (items.len() as u64) < min {
                    errors.push(format!("{here} requires at least {min} items, but got {}", items.len()));
                }
            }
            if let Some(max) = schema.get("maxItems").and_then(Value::as_u64) {
                if (items.len() as u64) > max {
                    errors.push(format!("{here} allows at most {max} items, but got {}", items.len()));
                }
            }
            if let Some(sub) = schema.get("items") {
                for (index, child) in items.iter().enumerate() {
                    queue.push_back((sub, child, format!("{path}/{index}")));
                }
            }
        }
        Value::String(text) => {
            // JSON Schema measures string length in Unicode code points, not bytes.
            let length = text.chars().count() as u64;
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
                if length < min {
                    errors.push(format!("{here} requires at least {min} characters, but got {length}"));
                }
            }
            if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
                if length > max {
                    errors.push(format!("{here} allows at most {max} characters, but got {length}"));
                }
            }
        }
        Value::Number(_) => {
            let number = value.as_f64().unwrap_or_default();
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
                if number < min {
                    errors.push(format!("{here} must not be less than {min}, but got {number}"));
                }
            }
            if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
                if number > max {
                    errors.push(format!("{here} must not be greater than {max}, but got {number}"));
                }
            }
        }
        _ => {}
    }
}

/// Equality for `enum` and `const`.
///
/// NOT `Value`'s own `PartialEq`: `serde_json::Number` compares by
/// REPRESENTATION, so `PosInt(2) != Float(2.0)`. That contradicts this module's
/// own `type: integer`, which accepts `2.0` on purpose — a child answering
/// `2.0` to `{"type":"integer","enum":[1,2,3]}` would pass the type gate, fail
/// the enum, be told only that its value is not allowed, resend the same
/// numerically-correct value, and burn every retry.
fn json_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => match (left.as_f64(), right.as_f64()) {
            // This also makes `-0.0` equal `0`, which is what a model comparing
            // against an enum of `0` expects.
            (Some(left), Some(right)) => left == right,
            _ => left == right,
        },
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left.iter().zip(right).all(|(left, right)| json_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, left)| right.get(key).is_some_and(|right| json_equal(left, right)))
        }
        _ => left == right,
    }
}

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n| n.fract() == 0.0)
        }
        _ => false,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Object(_) => "object",
        Value::Array(_) => "array",
        Value::String(_) => "string",
        Value::Bool(_) => "boolean",
        Value::Null => "null",
        Value::Number(_) => "number",
    }
}

fn schema_location(path: &str) -> String {
    if path.is_empty() {
        "output_schema root".to_string()
    } else {
        format!("output_schema {path}")
    }
}

fn instance_location(path: &str) -> String {
    if path.is_empty() {
        "output root".to_string()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object_schema(body: Value) -> Value {
        let mut map = body.as_object().expect("test schema is an object").clone();
        map.insert("type".into(), json!("object"));
        Value::Object(map)
    }

    fn compiled(body: Value) -> Schema {
        compile(&object_schema(body)).expect("schema compiles")
    }

    fn errors(schema: &Schema, instance: Value) -> Vec<String> {
        schema
            .validate(&instance)
            .expect_err("instance was expected to fail")
    }

    /// Each supported keyword has at least three valid and three invalid documents.
    #[test]
    fn every_supported_keyword_accepts_and_rejects_three_documents_each() {
        struct Case {
            keyword: &'static str,
            schema: Value,
            valid: [Value; 3],
            invalid: [Value; 3],
        }
        let cases = vec![
            Case {
                keyword: "type",
                schema: json!({"properties": {"a": {"type": "string"}}}),
                valid: [
                    json!({"a": ""}),
                    json!({"a": "x"}),
                    json!({"a": "很长的一段中文"}),
                ],
                invalid: [json!({"a": 1}), json!({"a": null}), json!({"a": ["x"]})],
            },
            Case {
                keyword: "properties",
                schema: json!({"properties": {"a": {"type": "integer"}, "b": {"type": "boolean"}}}),
                valid: [
                    json!({"a": 1, "b": true}),
                    json!({"a": -4}),
                    json!({"b": false, "c": "unconstrained"}),
                ],
                invalid: [
                    json!({"a": "1"}),
                    json!({"b": "true"}),
                    json!({"a": 1.5, "b": 0}),
                ],
            },
            Case {
                keyword: "required",
                schema: json!({"required": ["a", "b"]}),
                valid: [
                    json!({"a": 1, "b": 2}),
                    json!({"a": null, "b": null}),
                    json!({"a": {}, "b": [], "c": 3}),
                ],
                invalid: [json!({}), json!({"a": 1}), json!({"b": 2, "c": 3})],
            },
            Case {
                keyword: "items",
                schema: json!({"properties": {"a": {"type": "array", "items": {"type": "integer"}}}}),
                valid: [
                    json!({"a": []}),
                    json!({"a": [1]}),
                    json!({"a": [1, 2, 3]}),
                ],
                invalid: [
                    json!({"a": ["x"]}),
                    json!({"a": [1, "x"]}),
                    json!({"a": [null]}),
                ],
            },
            Case {
                keyword: "enum",
                schema: json!({"properties": {"a": {"enum": ["x", "y", 3]}}}),
                valid: [json!({"a": "x"}), json!({"a": "y"}), json!({"a": 3})],
                invalid: [json!({"a": "z"}), json!({"a": 4}), json!({"a": null})],
            },
            Case {
                keyword: "const",
                schema: json!({"properties": {"a": {"const": {"k": 1}}}}),
                valid: [
                    json!({"a": {"k": 1}}),
                    json!({"a": {"k": 1}, "b": 2}),
                    json!({}),
                ],
                invalid: [json!({"a": {"k": 2}}), json!({"a": {}}), json!({"a": 1})],
            },
            Case {
                keyword: "additionalProperties",
                schema: json!({"properties": {"a": {"type": "integer"}}, "additionalProperties": false}),
                valid: [json!({}), json!({"a": 1}), json!({"a": 2})],
                invalid: [
                    json!({"b": 1}),
                    json!({"a": 1, "b": 2}),
                    json!({"A": 1}),
                ],
            },
            Case {
                keyword: "minItems",
                schema: json!({"properties": {"a": {"type": "array", "minItems": 2}}}),
                valid: [
                    json!({"a": [1, 2]}),
                    json!({"a": [1, 2, 3]}),
                    json!({"a": ["x", null, {}]}),
                ],
                invalid: [json!({"a": []}), json!({"a": [1]}), json!({"a": ["x"]})],
            },
            Case {
                keyword: "maxItems",
                schema: json!({"properties": {"a": {"type": "array", "maxItems": 2}}}),
                valid: [json!({"a": []}), json!({"a": [1]}), json!({"a": [1, 2]})],
                invalid: [
                    json!({"a": [1, 2, 3]}),
                    json!({"a": [1, 2, 3, 4]}),
                    json!({"a": ["a", "b", "c"]}),
                ],
            },
            Case {
                keyword: "minLength",
                schema: json!({"properties": {"a": {"type": "string", "minLength": 3}}}),
                valid: [
                    json!({"a": "abc"}),
                    json!({"a": "abcd"}),
                    json!({"a": "中文字"}),
                ],
                invalid: [json!({"a": ""}), json!({"a": "ab"}), json!({"a": "中文"})],
            },
            Case {
                keyword: "maxLength",
                schema: json!({"properties": {"a": {"type": "string", "maxLength": 3}}}),
                valid: [json!({"a": ""}), json!({"a": "abc"}), json!({"a": "中文字"})],
                invalid: [
                    json!({"a": "abcd"}),
                    json!({"a": "中文字符"}),
                    json!({"a": "aaaaaa"}),
                ],
            },
            Case {
                keyword: "minimum",
                schema: json!({"properties": {"a": {"type": "number", "minimum": 0}}}),
                valid: [json!({"a": 0}), json!({"a": 1}), json!({"a": 0.5})],
                invalid: [json!({"a": -1}), json!({"a": -0.5}), json!({"a": -100})],
            },
            Case {
                keyword: "maximum",
                schema: json!({"properties": {"a": {"type": "number", "maximum": 10}}}),
                valid: [json!({"a": 10}), json!({"a": 0}), json!({"a": 9.5})],
                invalid: [json!({"a": 11}), json!({"a": 10.5}), json!({"a": 1e9})],
            },
        ];
        assert_eq!(
            cases.len(),
            SUPPORTED_KEYWORDS.len(),
            "every supported keyword must have one row in this table"
        );
        for case in cases {
            let schema = compiled(case.schema.clone());
            for instance in case.valid {
                assert!(
                    schema.validate(&instance).is_ok(),
                    "{} should accept {instance}",
                    case.keyword
                );
            }
            for instance in case.invalid {
                assert!(
                    schema.validate(&instance).is_err(),
                    "{} should reject {instance}",
                    case.keyword
                );
            }
        }
    }

    #[test]
    fn unsupported_constraint_keywords_are_rejected_at_compile_time() {
        for keyword in [
            "pattern",
            "format",
            "oneOf",
            "anyOf",
            "allOf",
            "not",
            "$ref",
            "patternProperties",
            "uniqueItems",
            "exclusiveMinimum",
            "multipleOf",
            "definitions",
        ] {
            let schema = object_schema(json!({
                "properties": {"a": {"type": "string", keyword: json!("whatever")}}
            }));
            let error = compile(&schema).expect_err("unsupported keyword must be rejected");
            assert!(
                error.contains(keyword),
                "{keyword} rejection message must name the keyword: {error}"
            );
            assert!(error.contains("/properties/a"), "message must include schema path: {error}");
        }
    }

    #[test]
    fn annotation_keywords_are_accepted_and_carry_no_constraint() {
        let schema = compile(&json!({
            "$schema": "https://json-schema.org/draft-07/schema#",
            "title": "结果",
            "description": "一次评审的结论",
            "type": "object",
            "properties": {
                "verdict": {"type": "string", "description": "结论", "examples": ["ok"]}
            },
            "required": ["verdict"]
        }))
        .expect("annotations compile");
        assert!(schema.validate(&json!({"verdict": "ok"})).is_ok());
        assert!(schema.validate(&json!({})).is_err());
    }

    #[test]
    fn the_top_level_must_be_an_object_schema() {
        assert!(compile(&json!({"type": "array", "items": {"type": "string"}}))
            .expect_err("array root")
            .contains("object"));
        assert!(compile(&json!({"properties": {}}))
            .expect_err("missing type")
            .contains("missing type"));
        assert!(compile(&json!([1, 2, 3]))
            .expect_err("array document")
            .contains("JSON object"));
        assert!(compile(&json!("x")).is_err());
    }

    #[test]
    fn node_and_depth_bounds_are_reported_with_the_size_message() {
        // Build the chain manually: `json!({"properties": {"a": deep}})` serializes
        // the interpolation through recursive `Value` serialization and would
        // exhaust the stack while arranging the test rather than in tested code.
        let mut deep = json!({"type": "string"});
        for _ in 0..(MAX_SCHEMA_DEPTH + 8) {
            let mut properties = serde_json::Map::new();
            properties.insert("a".into(), deep);
            let mut node = serde_json::Map::new();
            node.insert("type".into(), Value::String("object".into()));
            node.insert("properties".into(), Value::Object(properties));
            deep = Value::Object(node);
        }
        let error = compile(&deep).expect_err("depth bound");
        assert!(error.contains("too deep"), "{error}");
        assert!(error.contains(&MAX_SCHEMA_DEPTH.to_string()), "{error}");
        // `Value::drop` recurses natively, so dropping this 10,008-level chain
        // would exhaust the stack. Leaking it proves rejection occurs before any
        // recursive operation, as required by the MAX_SCHEMA_DEPTH contract.
        std::mem::forget(deep);

        let mut properties = serde_json::Map::new();
        // Each property contributes at least three nodes, so this exceeds the
        // node cap while remaining only four levels deep.
        for index in 0..MAX_SCHEMA_NODES {
            properties.insert(format!("p{index}"), json!({"type": "string"}));
        }
        let wide = json!({"type": "object", "properties": Value::Object(properties)});
        let error = compile(&wide).expect_err("node bound");
        assert!(error.contains("too large"), "{error}");
        assert!(error.contains(&MAX_SCHEMA_NODES.to_string()), "{error}");
    }

    #[test]
    fn schema_validity_precheck_rejects_malformed_constraint_values() {
        let cases = [
            (json!({"type": "object", "required": "a"}), "required"),
            (json!({"type": "object", "required": ["a", "a"]}), "duplicate"),
            (json!({"type": "object", "properties": []}), "properties"),
            (json!({"type": "object", "enum": []}), "enum"),
            (
                json!({"type": "object", "properties": {"a": {"type": "array", "items": [{"type": "string"}]}}}),
                "tuple",
            ),
            (
                json!({"type": "object", "properties": {"a": {"type": "string", "minLength": -1}}}),
                "non-negative integer",
            ),
            (
                json!({"type": "object", "properties": {"a": {"type": "string", "minLength": 5, "maxLength": 2}}}),
                "greater than",
            ),
            (
                json!({"type": "object", "properties": {"a": {"type": "number", "minimum": "0"}}}),
                "must be a number",
            ),
            (
                json!({"type": "object", "properties": {"a": {"type": "wat"}}}),
                "unsupported",
            ),
            (
                json!({"type": "object", "additionalProperties": 1}),
                "additionalProperties",
            ),
        ];
        for (schema, needle) in cases {
            let error = compile(&schema).expect_err("precheck must reject");
            assert!(error.contains(needle), "expected message to contain {needle}, got {error}");
        }
    }

    #[test]
    fn an_unsatisfiable_required_under_closed_properties_is_rejected() {
        let error = compile(&json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a", "b"],
            "additionalProperties": false
        }))
        .expect_err("unsatisfiable");
        assert!(error.contains("cannot be satisfied"), "{error}");
        // The same required list is valid on an open object.
        assert!(compile(&json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a", "b"]
        }))
        .is_ok());
    }

    #[test]
    fn validation_reports_every_error_with_an_instance_path() {
        let schema = compiled(json!({
            "properties": {
                "name": {"type": "string", "minLength": 2},
                "tags": {"type": "array", "items": {"type": "string"}},
                "score": {"type": "integer", "minimum": 0, "maximum": 10},
                "summary": {"type": "string"}
            },
            "required": ["name", "score", "summary"]
        }));
        let found = errors(
            &schema,
            json!({"name": "a", "tags": ["ok", 7, false], "score": 42}),
        );
        assert!(
            found.iter().any(|e| e.starts_with("/name")),
            "缺少 /name 的错误：{found:?}"
        );
        assert!(
            found.iter().any(|e| e.starts_with("/tags/1")),
            "缺少 /tags/1 的错误：{found:?}"
        );
        assert!(
            found.iter().any(|e| e.starts_with("/tags/2")),
            "缺少 /tags/2 的错误：{found:?}"
        );
        assert!(
            found.iter().any(|e| e.starts_with("/score")),
            "缺少 /score 的错误：{found:?}"
        );
        assert!(
            found.iter().any(|e| e.contains("missing required field")),
            "missing required-field error: {found:?}"
        );
        let message = Schema::validation_message(&found);
        assert!(message.starts_with("Structured output does not conform to output_schema:"));
        assert_eq!(message.matches("\n- ").count(), found.len());
    }

    #[test]
    fn a_wrong_type_does_not_cascade_into_derived_range_errors() {
        let schema = compiled(json!({
            "properties": {"a": {"type": "string", "minLength": 3, "maxLength": 4}}
        }));
        let found = errors(&schema, json!({"a": 1}));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("expected string"), "{found:?}");
    }

    #[test]
    fn integer_accepts_a_whole_float_but_not_a_fraction() {
        let schema = compiled(json!({"properties": {"a": {"type": "integer"}}}));
        assert!(schema.validate(&json!({"a": 3})).is_ok());
        assert!(schema.validate(&json!({"a": 3.0})).is_ok());
        assert!(schema.validate(&json!({"a": -3})).is_ok());
        assert!(schema.validate(&json!({"a": 3.5})).is_err());
    }

    #[test]
    fn string_lengths_count_code_points_not_bytes() {
        let schema = compiled(json!({
            "properties": {"a": {"type": "string", "maxLength": 3}}
        }));
        // Three Han characters occupy nine UTF-8 bytes but only three code points.
        assert!(schema.validate(&json!({"a": "中文字"})).is_ok());
        assert!(schema.validate(&json!({"a": "中文字符"})).is_err());
    }

    #[test]
    fn a_deeply_nested_instance_does_not_recurse_natively() {
        // The schema is shallow but the instance is long; validation must use
        // the explicit queue rather than grow native stack frames with depth.
        let schema = compiled(json!({
            "properties": {"a": {"type": "array", "items": {"type": "integer"}}}
        }));
        let items: Vec<Value> = (0..50_000).map(|n| json!(n)).collect();
        assert!(schema.validate(&json!({"a": items})).is_ok());
    }

    #[test]
    fn error_collection_is_capped_and_says_so() {
        let schema = compiled(json!({
            "properties": {"a": {"type": "array", "items": {"type": "string"}}}
        }));
        let items: Vec<Value> = (0..(MAX_VALIDATION_ERRORS * 3)).map(|n| json!(n)).collect();
        let found = errors(&schema, json!({"a": items}));
        assert!(found.len() <= MAX_VALIDATION_ERRORS + 1, "{}", found.len());
        assert!(found.last().expect("some errors").contains("Too many errors"));
    }

    /// The cap has to hold when ONE node produces all the errors, not only when
    /// each error comes from its own queue entry. The whole list is returned to
    /// the child verbatim as tool-result text, so an uncapped list turns a 32 KiB
    /// input into hundreds of KiB of context — five times over, once per retry.
    #[test]
    fn a_single_wide_node_cannot_blow_past_the_error_cap() {
        let closed = compiled(json!({
            "properties": {"result": {"type": "string"}},
            "additionalProperties": false
        }));
        let mut instance = serde_json::Map::new();
        for index in 0..(MAX_VALIDATION_ERRORS * 30) {
            instance.insert(format!("k{index:05}"), json!(0));
        }
        let found = errors(&closed, Value::Object(instance));
        assert!(found.len() <= MAX_VALIDATION_ERRORS + 1, "{}", found.len());
        assert!(found.last().expect("some errors").contains("Too many errors"));

        // Same shape through `required` on an OPEN object, which
        // `check_cross_keyword` deliberately does not reject.
        let names: Vec<Value> = (0..(MAX_VALIDATION_ERRORS * 30))
            .map(|index| json!(format!("k{index:05}")))
            .collect();
        let demanding = compiled(json!({"required": Value::Array(names)}));
        let found = errors(&demanding, json!({}));
        assert!(found.len() <= MAX_VALIDATION_ERRORS + 1, "{}", found.len());
        assert!(found.last().expect("some errors").contains("Too many errors"));
    }

    /// `serde_json::Number`'s own `PartialEq` compares by representation, so
    /// `2 != 2.0`. Left alone that contradicts this module's `type: integer`,
    /// which accepts `2.0` — the child would pass the type gate, fail the enum
    /// on a numerically-correct answer, and burn every retry resending it.
    #[test]
    fn enum_and_const_compare_numbers_by_value_not_by_representation() {
        let schema = compiled(json!({
            "properties": {"score": {"type": "integer", "enum": [1, 2, 3]}}
        }));
        assert!(schema.validate(&json!({"score": 2})).is_ok());
        assert!(schema.validate(&json!({"score": 2.0})).is_ok());
        assert!(schema.validate(&json!({"score": 4})).is_err());
        assert!(schema.validate(&json!({"score": 2.5})).is_err());

        let zero = compiled(json!({"properties": {"a": {"enum": [0]}}}));
        assert!(zero.validate(&json!({"a": -0.0})).is_ok());

        let konst = compiled(json!({"properties": {"a": {"const": 5}}}));
        assert!(konst.validate(&json!({"a": 5.0})).is_ok());
        assert!(konst.validate(&json!({"a": 6})).is_err());

        // Nested containers compare through the same rule.
        let nested = compiled(json!({
            "properties": {"a": {"const": {"k": [1, 2]}}}
        }));
        assert!(nested.validate(&json!({"a": {"k": [1.0, 2.0]}})).is_ok());
        assert!(nested.validate(&json!({"a": {"k": [1, 2, 3]}})).is_err());
        assert!(nested.validate(&json!({"a": {"k": [1, 2], "extra": 1}})).is_err());
        // Type still matters: a string 2 is not the number 2.
        let strict = compiled(json!({"properties": {"a": {"enum": [2]}}}));
        assert!(strict.validate(&json!({"a": "2"})).is_err());
    }

    #[test]
    fn additional_properties_as_a_subschema_constrains_the_extras() {
        let schema = compiled(json!({
            "properties": {"a": {"type": "string"}},
            "additionalProperties": {"type": "integer"}
        }));
        assert!(schema.validate(&json!({"a": "x", "b": 1})).is_ok());
        let found = errors(&schema, json!({"a": "x", "b": "1"}));
        assert!(found.iter().any(|e| e.starts_with("/b")), "{found:?}");
    }

    #[test]
    fn a_compiled_schema_round_trips_the_original_document() {
        let document = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a"]
        });
        let schema = compile(&document).expect("compiles");
        assert_eq!(schema.as_value(), &document);
        assert_eq!(schema.clone().into_value(), document);
    }
}
