//! Defines which tools enter a request and where their parameter schemas come from.
//!
//! This module contains only [`enabled_tools`] and [`tool_schema`]'s four-level
//! per-model schema-source precedence. Native server-side search has no
//! `ToolDescriptor`; its gate is in [`super::step`] and its sidecar definition
//! is in `search.ts`.
//!
//! Both are host policy, independent of wire format. The AI SDK sidecar wraps
//! `{name, description, inputSchema}` for each provider protocol.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::model::{RunModelRequest, ToolDescriptor, ToolParameterType};
use crate::prompt_profile::PromptProfile;

pub(crate) fn enabled_tools(request: &RunModelRequest) -> Vec<&ToolDescriptor> {
    let enabled = request.enabled_tools.iter().collect::<HashSet<_>>();
    request
        .tools
        .iter()
        // Dangerous tools are advertised because every individual call is gated by the
        // native approval callback before execution. Orchestration tools are advertised
        // too: the run loop executes them itself (subagent/ask_user) or via the pure
        // host-side executor (Task*); subagent runs exclude them from this list.
        .filter(|tool| enabled.contains(&tool.name))
        .collect()
}

pub(crate) fn tool_schema(
    tool: &ToolDescriptor,
    supports_vision: bool,
    profile: &PromptProfile,
) -> Value {
    // Schema-source precedence:
    // 1. Pass descriptor-provided `input_schema` through verbatim.
    // 2. Built-in catalog tools use authoritative hand-written schemas, whose
    //    root description is the run profile's text for that tool.
    // 3. Legacy `memory_*` aliases retain their original schemas.
    // 4. Derive schemas from typed parameters for remaining internal descriptors.
    if let Some(schema) = &tool.input_schema {
        return schema.clone();
    }
    if let Some(schema) = crate::builtin_schemas::builtin_tool_schema(&tool.name, profile) {
        return without_unusable_browser_actions(schema, supports_vision);
    }
    if let Some(schema) = memory_tool_schema(&tool.name) {
        return schema;
    }

    let mut properties = Map::new();
    let mut required = Vec::new();
    for parameter in &tool.parameters {
        let mut schema = match parameter.parameter_type {
            ToolParameterType::String | ToolParameterType::Multiline => json!({"type": "string"}),
            ToolParameterType::Number => json!({"type": "number"}),
            ToolParameterType::Boolean => json!({"type": "boolean"}),
            ToolParameterType::Json => json!({}),
        };
        if let Some(help) = &parameter.help {
            schema["description"] = json!(help);
        }
        if let Some(default) = &parameter.default_value {
            schema["default"] = default.clone();
        }
        properties.insert(parameter.name.clone(), schema);
        if parameter.required {
            required.push(parameter.name.clone());
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Drops the browser actions a text-only model can never use from the
/// model-facing schema.
///
/// The static schema stays complete: it is the authoritative catalog baseline,
/// and every action keeps its runtime dispatch and security policy. What changes
/// is only what this request advertises. Filtering here rather than after the
/// fact is the point — the host used to let a text-only model call `screenshot`,
/// capture the page, write the PNG, and only then replace the result with an
/// error saying the model cannot see images, so the work happened and the answer
/// never arrived.
fn without_unusable_browser_actions(schema: Value, supports_vision: bool) -> Value {
    use crate::browser::PlaywrightAction;

    if supports_vision {
        return schema;
    }
    let Value::Object(mut object) = schema else {
        return schema;
    };
    let Some(Value::Array(variants)) = object.remove("oneOf") else {
        return Value::Object(object);
    };
    let kept: Vec<Value> = variants
        .into_iter()
        .filter(|variant| {
            let action = variant
                .get("properties")
                .and_then(|properties| properties.get("action"))
                .and_then(|action| action.get("const"))
                .and_then(Value::as_str);
            let Some(action) = action else {
                // An unrecognized variant is left in place: this filter removes a
                // known-unusable action, it does not decide what is legitimate.
                return true;
            };
            !PlaywrightAction::ALL
                .iter()
                .any(|candidate| candidate.as_str() == action && candidate.requires_image_capability())
        })
        .collect();
    object.insert("oneOf".into(), Value::Array(kept));
    Value::Object(object)
}

fn memory_tool_schema(name: &str) -> Option<Value> {
    let scope = || {
        json!({
            "type": "string",
            "enum": ["project", "global"],
            "default": "project",
            "description": "Applicability scope only. Mework injects the exact model identity and current project; neither is accepted as an argument."
        })
    };
    let document_name = || {
        json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 80,
            "description": "MEMORY.md or a safe relative topic path such as topics/debugging.md. Absolute paths, backslashes, empty segments, and . or .. segments are not accepted."
        })
    };
    let expected_version = || {
        json!({
            "type": "integer",
            "minimum": 0,
            "description": "Required compare-and-swap version from memory_read. Use 0 for create; a mismatch is rejected."
        })
    };
    Some(match name {
        "memory_list" => json!({
            "type": "object",
            "properties": {"scope": scope()},
            "additionalProperties": false
        }),
        "memory_read" => json!({
            "type": "object",
            "properties": {
                "scope": scope(),
                "name": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 80,
                    "default": "MEMORY.md",
                    "description": "MEMORY.md or a safe relative topic path such as topics/debugging.md. Absolute paths, backslashes, empty segments, and . or .. segments are not accepted."
                }
            },
            "additionalProperties": false
        }),
        "memory_search" => json!({
            "type": "object",
            "properties": {
                "scope": scope(),
                "query": {"type": "string", "minLength": 1, "maxLength": 1000},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 20}
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        "memory_upsert" => json!({
            "type": "object",
            "properties": {
                "scope": scope(),
                "name": document_name(),
                "content": {"type": "string", "maxLength": 262144},
                "expected_version": expected_version()
            },
            "required": ["name", "content", "expected_version"],
            "additionalProperties": false
        }),
        "memory_delete" => json!({
            "type": "object",
            "properties": {
                "scope": scope(),
                "name": document_name(),
                "expected_version": expected_version()
            },
            "required": ["name", "expected_version"],
            "additionalProperties": false
        }),
        _ => return None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::PlaywrightAction;

    fn advertised_actions(supports_vision: bool) -> Vec<String> {
        let schema = crate::builtin_schemas::builtin_tool_schema(
            "playwright",
            &PromptProfile::builtin_english(),
        )
        .expect("playwright has a built-in schema");
        without_unusable_browser_actions(schema, supports_vision)["oneOf"]
            .as_array()
            .expect("playwright is a variant union")
            .iter()
            .filter_map(|variant| variant["properties"]["action"]["const"].as_str())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn text_only_models_exclude_only_image_browser_actions() {
        let seeing = advertised_actions(true);
        let text_only = advertised_actions(false);
        assert_eq!(seeing.len(), PlaywrightAction::ALL.len());

        let removed: Vec<&String> = seeing
            .iter()
            .filter(|action| !text_only.contains(action))
            .collect();
        assert_eq!(removed, vec!["screenshot", "upload_image"]);
        assert!(text_only.iter().all(|action| {
            action != "screenshot" && action != "upload_image"
        }));
    }

    #[test]
    fn a_schema_the_filter_does_not_recognize_passes_through() {
        let plain = json!({"type": "object", "properties": {"path": {"type": "string"}}});
        assert_eq!(without_unusable_browser_actions(plain.clone(), false), plain);

        let opaque = json!({"oneOf": [{"type": "object"}]});
        assert_eq!(without_unusable_browser_actions(opaque.clone(), false), opaque);
    }
}
