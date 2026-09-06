//! A deliberately small JSON Schema validator, and a check that keeps it honest.
//!
//! Pipeline step 3 validates a model's tool arguments before any policy sees them, so a
//! policy always reads well-formed input. Doing that with a full JSON Schema
//! implementation means a large dependency tree behind a security check; doing it with a
//! hand-written validator means a vocabulary that is smaller than JSON Schema's.
//!
//! The dangerous version of "smaller" is a validator that **ignores** what it does not
//! understand: a schema advertising `"pattern"` would then be advertising a constraint
//! nothing enforces, which is exactly the failure the deny list had before it became a
//! glob set. So the vocabulary is closed in the other direction — an unsupported keyword
//! is rejected at **tool registration**, by [`validate_spec`], and the tool never loads.
//!
//! Supported: `type` (single or array), `properties`, `required`, `additionalProperties`
//! (boolean), `items`, `enum`, `const`, `minimum`, `maximum`, `exclusiveMinimum`,
//! `exclusiveMaximum`, `minLength`, `maxLength`, `minItems`, `maxItems`.
//! Accepted and ignored, because they are annotations rather than constraints: `title`,
//! `description`, `$schema`, `$id`, `examples`, `default`, `deprecated`, `readOnly`.

use rivet_core::error::Error;
use rivet_core::tool::ToolSpec;
use serde_json::Value;

/// Keywords the validator enforces.
const ENFORCED: [&str; 14] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "minItems",
];

/// Keywords that document rather than constrain, and are safe to ignore.
const ANNOTATIONS: [&str; 8] = [
    "title",
    "description",
    "$schema",
    "$id",
    "examples",
    "default",
    "deprecated",
    "readOnly",
];

/// Every type name the validator understands.
const TYPES: [&str; 7] = [
    "object", "array", "string", "number", "integer", "boolean", "null",
];

/// Check a tool's declared schema before the tool is registered.
///
/// # Errors
/// [`rivet_core::error::ErrorKind::InvalidArgument`] when the schema uses a keyword this
/// validator does not enforce. Loading the tool anyway would mean shipping a constraint
/// that silently does nothing.
pub fn validate_spec(spec: &ToolSpec) -> rivet_core::Result<()> {
    let mut unsupported = Vec::new();
    check_vocabulary(&spec.input_schema, "input_schema", &mut unsupported);
    if unsupported.is_empty() {
        return Ok(());
    }
    Err(Error::invalid_argument(format!(
        "tool `{}` declares schema keywords the runtime does not enforce: {}. \
         Extend `rivet_runtime::schema` deliberately rather than shipping a constraint \
         that does nothing.",
        spec.name,
        unsupported.join(", ")
    )))
}

fn check_vocabulary(schema: &Value, path: &str, unsupported: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        // A non-object subschema (`true`/`false`) is not part of the vocabulary.
        unsupported.push(format!("{path}: subschema must be an object"));
        return;
    };

    for (key, value) in object {
        if ANNOTATIONS.contains(&key.as_str()) {
            continue;
        }
        // `minItems`/`maxItems` share a check; listing both in ENFORCED would be
        // redundant, so `maxItems` is matched here.
        if !ENFORCED.contains(&key.as_str()) && key != "maxItems" {
            unsupported.push(format!("{path}.{key}"));
            continue;
        }
        match key.as_str() {
            "properties" => {
                if let Some(properties) = value.as_object() {
                    for (name, subschema) in properties {
                        check_vocabulary(
                            subschema,
                            &format!("{path}.properties.{name}"),
                            unsupported,
                        );
                    }
                }
            }
            "items" => check_vocabulary(value, &format!("{path}.items"), unsupported),
            "type" => {
                let names: Vec<&str> = match value {
                    Value::String(name) => vec![name.as_str()],
                    Value::Array(values) => values.iter().filter_map(Value::as_str).collect(),
                    _ => Vec::new(),
                };
                if names.is_empty() || names.iter().any(|n| !TYPES.contains(n)) {
                    unsupported.push(format!("{path}.type = {value}"));
                }
            }
            "additionalProperties" => {
                if !value.is_boolean() {
                    unsupported.push(format!("{path}.additionalProperties (must be a boolean)"));
                }
            }
            _ => {}
        }
    }
}

/// Validate a value against a schema.
///
/// # Errors
/// Every failure, as one message per problem, phrased for the model that has to fix it:
/// `input.path: expected string, got number`.
pub fn validate(schema: &Value, input: &Value) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    check(schema, input, "input", &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[allow(clippy::too_many_lines)] // One keyword per arm; splitting it hides the vocabulary.
fn check(schema: &Value, value: &Value, path: &str, problems: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        return;
    };

    if let Some(allowed) = object.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        problems.push(format!(
            "{path}: must be one of {}",
            Value::Array(allowed.clone())
        ));
    }
    if let Some(expected) = object.get("const")
        && expected != value
    {
        problems.push(format!("{path}: must be {expected}"));
    }

    if let Some(types) = type_names(object.get("type"))
        && !types.iter().any(|name| matches_type(name, value))
    {
        problems.push(format!(
            "{path}: expected {}, got {}",
            types.join(" or "),
            describe(value)
        ));
        // Once the type is wrong, the keyword checks below would only add noise.
        return;
    }

    match value {
        Value::Object(fields) => {
            for required in object
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !fields.contains_key(required) {
                    problems.push(format!("{path}.{required}: required but missing"));
                }
            }
            let properties = object.get("properties").and_then(Value::as_object);
            if object.get("additionalProperties") == Some(&Value::Bool(false))
                && let Some(properties) = properties
            {
                for name in fields.keys() {
                    if !properties.contains_key(name) {
                        problems.push(format!("{path}.{name}: unknown property"));
                    }
                }
            }
            if let Some(properties) = properties {
                for (name, subschema) in properties {
                    if let Some(field) = fields.get(name) {
                        check(subschema, field, &format!("{path}.{name}"), problems);
                    }
                }
            }
        }
        Value::Array(items) => {
            check_bound(
                object,
                "minItems",
                items.len(),
                path,
                "items",
                true,
                problems,
            );
            check_bound(
                object,
                "maxItems",
                items.len(),
                path,
                "items",
                false,
                problems,
            );
            if let Some(subschema) = object.get("items") {
                for (index, item) in items.iter().enumerate() {
                    check(subschema, item, &format!("{path}[{index}]"), problems);
                }
            }
        }
        Value::String(text) => {
            let length = text.chars().count();
            check_bound(
                object,
                "minLength",
                length,
                path,
                "characters",
                true,
                problems,
            );
            check_bound(
                object,
                "maxLength",
                length,
                path,
                "characters",
                false,
                problems,
            );
        }
        Value::Number(number) => {
            if let Some(actual) = number.as_f64() {
                for (key, ok, rendered) in [
                    ("minimum", actual >= bound(object, "minimum"), "at least"),
                    ("maximum", actual <= bound(object, "maximum"), "at most"),
                    (
                        "exclusiveMinimum",
                        actual > bound(object, "exclusiveMinimum"),
                        "greater than",
                    ),
                    (
                        "exclusiveMaximum",
                        actual < bound(object, "exclusiveMaximum"),
                        "less than",
                    ),
                ] {
                    if let Some(limit) = object.get(key)
                        && !ok
                    {
                        problems.push(format!("{path}: must be {rendered} {limit}"));
                    }
                }
            }
        }
        Value::Bool(_) | Value::Null => {}
    }
}

/// A numeric keyword's value, or a bound that cannot be violated when it is absent.
fn bound(object: &serde_json::Map<String, Value>, key: &str) -> f64 {
    let neutral = match key {
        "minimum" | "exclusiveMinimum" => f64::NEG_INFINITY,
        _ => f64::INFINITY,
    };
    object.get(key).and_then(Value::as_f64).unwrap_or(neutral)
}

fn check_bound(
    object: &serde_json::Map<String, Value>,
    key: &str,
    actual: usize,
    path: &str,
    unit: &str,
    is_minimum: bool,
    problems: &mut Vec<String>,
) {
    let Some(limit) = object.get(key).and_then(Value::as_u64) else {
        return;
    };
    let actual = actual as u64;
    let violated = if is_minimum {
        actual < limit
    } else {
        actual > limit
    };
    if violated {
        let word = if is_minimum { "at least" } else { "at most" };
        problems.push(format!(
            "{path}: must have {word} {limit} {unit}, got {actual}"
        ));
    }
}

fn type_names(value: Option<&Value>) -> Option<Vec<&str>> {
    match value? {
        Value::String(name) => Some(vec![name.as_str()]),
        Value::Array(values) => Some(values.iter().filter_map(Value::as_str).collect()),
        _ => None,
    }
}

fn matches_type(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        // JSON has one number type; "integer" means a number with no fractional part.
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(schema: Value) -> ToolSpec {
        ToolSpec::new("t", "a tool", schema).unwrap()
    }

    fn read_file_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 10_000 }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    #[test]
    fn a_valid_input_passes() {
        assert!(
            validate(
                &read_file_schema(),
                &serde_json::json!({ "path": "src/main.rs", "limit": 100 })
            )
            .is_ok()
        );
    }

    #[test]
    fn a_missing_required_field_is_named() {
        let problems = validate(&read_file_schema(), &serde_json::json!({})).unwrap_err();
        assert_eq!(problems, ["input.path: required but missing"]);
    }

    #[test]
    fn a_wrong_type_reads_like_something_the_model_can_fix() {
        let problems =
            validate(&read_file_schema(), &serde_json::json!({ "path": 7 })).unwrap_err();
        assert_eq!(problems, ["input.path: expected string, got number"]);
    }

    #[test]
    fn truncated_arguments_are_rejected_rather_than_run() {
        // What a `finish_reason: "length"` leaves behind: the adapter hands the raw string
        // through and this is where it stops.
        let raw = serde_json::Value::String("{\"path\": \"src/li".into());
        let problems = validate(&read_file_schema(), &raw).unwrap_err();
        assert_eq!(problems, ["input: expected object, got string"]);
    }

    #[test]
    fn unknown_properties_are_rejected_when_the_schema_says_so() {
        let problems = validate(
            &read_file_schema(),
            &serde_json::json!({ "path": "a", "recursive": true }),
        )
        .unwrap_err();
        assert_eq!(problems, ["input.recursive: unknown property"]);
    }

    #[test]
    fn numeric_and_length_bounds_are_enforced() {
        let problems = validate(
            &read_file_schema(),
            &serde_json::json!({ "path": "", "limit": 0 }),
        )
        .unwrap_err();
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems
                .iter()
                .any(|p| p == "input.path: must have at least 1 characters, got 0"),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p == "input.limit: must be at least 1"),
            "{problems:?}"
        );
    }

    #[test]
    fn enums_and_arrays_are_checked() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "mode": { "enum": ["read", "write"] },
                "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
            }
        });
        assert!(
            validate(
                &schema,
                &serde_json::json!({ "mode": "read", "paths": ["a"] })
            )
            .is_ok()
        );
        let problems = validate(
            &schema,
            &serde_json::json!({ "mode": "delete", "paths": [1] }),
        )
        .unwrap_err();
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[1].contains("input.paths[0]"), "{problems:?}");
    }

    #[test]
    fn an_integer_keyword_rejects_a_fraction() {
        let schema =
            serde_json::json!({ "type": "object", "properties": { "n": { "type": "integer" } } });
        assert!(validate(&schema, &serde_json::json!({ "n": 1.5 })).is_err());
        assert!(validate(&schema, &serde_json::json!({ "n": 2 })).is_ok());
    }

    #[test]
    fn a_supported_schema_registers() {
        assert!(validate_spec(&spec(read_file_schema())).is_ok());
    }

    #[test]
    fn an_unsupported_keyword_is_refused_at_registration() {
        // A schema advertising `pattern` while nothing enforces it is worse than one that
        // never claimed it. Same lesson as the deny list.
        let err = validate_spec(&spec(serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string", "pattern": "^src/" } }
        })))
        .unwrap_err();
        assert!(
            err.message()
                .contains("input_schema.properties.path.pattern"),
            "{err}"
        );
    }

    #[test]
    fn composition_keywords_are_refused_too() {
        for keyword in ["anyOf", "oneOf", "allOf", "$ref", "not"] {
            let err = validate_spec(&spec(serde_json::json!({
                "type": "object",
                keyword: []
            })))
            .unwrap_err();
            assert!(err.message().contains(keyword), "{keyword}: {err}");
        }
    }

    #[test]
    fn an_unknown_type_name_is_refused() {
        let err = validate_spec(&spec(serde_json::json!({ "type": "tuple" }))).unwrap_err();
        assert!(err.message().contains("type"), "{err}");
    }

    #[test]
    fn annotations_are_allowed_and_ignored() {
        assert!(
            validate_spec(&spec(serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "title": "read_file input",
                "description": "what to read",
                "type": "object",
                "properties": { "path": { "type": "string", "default": "." } }
            })))
            .is_ok()
        );
    }
}
