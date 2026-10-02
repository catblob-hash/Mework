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
    let supports_vision = request.model.supports_vision();
    // Derived here rather than read from `enabled_tools`: the plan pair follows
    // the conversation's plan-mode switch, never a name the renderer wrote.
    let plan_tools = crate::plan_mode::derived_tools(request.plan_tools, request.subagent_depth);
    // Same for the handoff tools, which the run arms mid-turn once the context
    // crosses the auto-compact threshold.
    let handoff_tools = crate::handoff::derived_tools(request);
    request
        .tools
        .iter()
        // Dangerous tools are advertised because every individual call is gated by the
        // native approval callback before execution. Orchestration tools are advertised
        // too: the run loop executes them itself (subagent/ask_user) or via the pure
        // host-side executor (Task*); subagent runs exclude them from this list.
        .filter(|tool| {
            enabled.contains(&tool.name)
                || plan_tools.contains(&tool.name.as_str())
                || handoff_tools.contains(&tool.name.as_str())
        })
        .filter(|tool| supports_vision || is_usable_without_vision(&tool.name))
        // A shell tool is advertised only where its shell is: the tool list a
        // conversation shows is the union of its machines' backends, and a
        // backend none of this run's machines has — PowerShell on a Mac, zsh
        // on Windows — has nowhere to run. Withdrawing it is the honest form:
        // the user may well have it enabled, and advertising it would buy one
        // wasted call and one error per turn until the model stopped trying.
        .filter(|tool| {
            crate::shell_backend::ShellBackend::of_tool(&tool.name)
                .map_or(true, |backend| runs_backend(&request.workspaces, backend))
        })
        .collect()
}

/// Whether a run's workspaces include one whose machine has `backend`.
///
/// A run that resolved no workspace set predates machine-bound workspaces, so it
/// is on the host machine and the host's own shells answer.
fn runs_backend(
    workspaces: &crate::workspace_set::WorkspaceSet,
    backend: crate::shell_backend::ShellBackend,
) -> bool {
    if workspaces.is_empty() {
        return crate::machine_shells::local()
            .get(backend)
            .is_some();
    }
    workspaces.runs(backend)
}

/// Whether a tool means anything to a model that cannot see images.
///
/// `preview_screenshot` hands back pixels and `preview_upload_image` sends an image the
/// conversation could only be carrying for such a model. Before this gate the host would capture
/// the screenshot and only then replace the result with "this model cannot see images" — the work
/// happened and the answer never arrived. Two whole tools now, not two variants of one.
pub(crate) fn is_usable_without_vision(tool_name: &str) -> bool {
    crate::browser::PreviewTool::from_tool_name(tool_name)
        .is_none_or(|tool| !tool.requires_image_capability())
}

pub(crate) fn tool_schema(
    tool: &ToolDescriptor,
    profile: &PromptProfile,
    workspaces: &crate::workspace_set::WorkspaceSet,
) -> Value {
    // Schema-source precedence:
    // 1. Pass descriptor-provided `input_schema` through verbatim.
    // 2. Built-in catalog tools use authoritative hand-written schemas, whose
    //    root description is the run profile's text for that tool.
    // 3. Legacy `memory_*` aliases retain their original schemas.
    // 4. Derive schemas from typed parameters for remaining internal descriptors.
    //
    // The `workspace` parameter is applied on top of whichever rung answered: a
    // conversation's workspaces are a property of the run, not of the rung the
    // schema came from, and a descriptor injected with roles must still be able
    // to say which directory it acts in.
    if let Some(schema) = &tool.input_schema {
        return crate::builtin_schemas::with_workspace_parameter(
            schema.clone(),
            &tool.name,
            workspaces,
        );
    }
    if let Some(schema) = crate::builtin_schemas::builtin_tool_schema(&tool.name, profile) {
        return crate::builtin_schemas::with_workspace_parameter(schema, &tool.name, workspaces);
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

    /// A text-only model must not be offered a tool whose whole point is an image. Before this
    /// gate the host would capture the screenshot and only then replace the result with "this
    /// model cannot see images" — the side effect happened and the model learned nothing.
    #[test]
    fn text_only_models_are_offered_neither_image_preview_tool() {
        let removed = ["preview_screenshot", "preview_upload_image"];
        for tool in crate::catalog::tool_catalog() {
            assert_eq!(
                is_usable_without_vision(&tool.name),
                !removed.contains(&tool.name.as_str()),
                "{}",
                tool.name
            );
        }
        // Everything else the preview surface can do is still offered.
        assert!(is_usable_without_vision("preview_snapshot"));
        assert!(is_usable_without_vision("preview_click"));
        assert!(is_usable_without_vision("read"));

        // The static catalog schema itself stays complete: it is the authoritative baseline, and
        // both tools keep their runtime dispatch and security policy.
        for name in removed {
            assert!(crate::builtin_schemas::builtin_tool_schema(
                name,
                &PromptProfile::builtin_english()
            )
            .is_some());
        }
    }

    /// A conversation whose machines lack a shell has nowhere to run that
    /// shell's tools, and advertising them there buys one wasted call and one
    /// error per turn until the model stops trying. The catalog stays complete
    /// either way: withdrawal is a property of the run, not of the tool.
    #[test]
    fn a_shell_is_offered_only_where_a_workspace_could_run_it() {
        use crate::model::{AttachedWorkspace, RunTarget, SshMachineConfig};
        use crate::shell_backend::ShellBackend;
        use crate::workspace_set::WorkspaceSet;

        let assets = crate::model::ExecutionEnvironmentAssets {
            ssh_machines: vec![SshMachineConfig {
                id: "m1".into(),
                name: "devbox".into(),
                host: "user@devbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let remote = AttachedWorkspace {
            machine: Some(RunTarget::Ssh {
                machine_id: "m1".into(),
            }),
            path: "~/app".into(),
        };

        // An unprobed SSH machine has bash and nothing else.
        let set = WorkspaceSet::resolve(&assets, &remote, &[]).unwrap();
        assert!(!runs_backend(&set, ShellBackend::PowerShell));
        assert!(runs_backend(&set, ShellBackend::Bash));
        // A host workspace brings back exactly the host's own shells.
        let local = crate::machine_shells::local();
        for backend in ShellBackend::ALL {
            assert_eq!(
                runs_backend(&WorkspaceSet::local_root("C:/work/app"), backend),
                local.get(backend).is_some(),
                "{backend}"
            );
            // A run that resolved no set predates machine-bound workspaces, so
            // the host's own shells answer rather than a silent withdrawal.
            assert_eq!(
                runs_backend(&WorkspaceSet::default(), backend),
                local.get(backend).is_some(),
                "{backend}"
            );
        }
        assert_eq!(
            runs_backend(&WorkspaceSet::default(), ShellBackend::PowerShell),
            crate::host_platform::host_platform().is_windows()
        );
    }
}
