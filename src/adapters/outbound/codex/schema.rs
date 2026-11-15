//! Schema compatibility rules ported from raine/claude-code-proxy (MIT).
//! Source: fe107180f7a69c6d63dd4421a6567cf641c1f103, translate/request.rs.
//! Copyright (c) 2026 Raine Virta; license retained in LICENSE.
use serde_json::Value;

pub(super) fn strip_tool_schema_patterns(schema: &mut Value) {
    let Some(schema) = schema.as_object_mut() else {
        return;
    };
    schema.remove("pattern");

    for (keyword, value) in schema {
        match keyword.as_str() {
            "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
            | "dependencies" => {
                if let Some(schemas) = value.as_object_mut() {
                    for schema in schemas.values_mut() {
                        strip_tool_schema_patterns(schema);
                    }
                }
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" | "items" => {
                if let Some(schemas) = value.as_array_mut() {
                    for schema in schemas {
                        strip_tool_schema_patterns(schema);
                    }
                } else if keyword == "items" {
                    strip_tool_schema_patterns(value);
                }
            }
            "additionalProperties"
            | "additionalItems"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "contains"
            | "propertyNames"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contentSchema" => strip_tool_schema_patterns(value),
            _ => {}
        }
    }
}

pub(super) fn normalize_strict_json_schema(schema: &Value) -> Value {
    match schema {
        Value::Array(arr) => Value::Array(arr.iter().map(normalize_strict_json_schema).collect()),
        Value::Object(map) => {
            let mut out = map.clone();
            if let Some(properties) = out.get("properties").and_then(|v| v.as_object()) {
                let keys: Vec<String> = properties.keys().cloned().collect();
                out.insert(
                    "required".into(),
                    Value::Array(keys.into_iter().map(Value::String).collect()),
                );
            }
            for (key, val) in out.clone().iter() {
                out.insert(key.clone(), normalize_strict_json_schema(val));
            }
            Value::Object(out)
        }
        _ => schema.clone(),
    }
}
