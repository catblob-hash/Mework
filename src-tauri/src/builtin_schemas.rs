//! Authoritative model-visible JSON Schemas for built-in tools.
//!
//! Tool descriptions are advisory usage text and are empty in seeds unless supplied by
//! `.mework/tool-descriptions`; schemas define what a tool is.
//!
//! Express parameter and cross-field boundaries with JSON Schema keywords. Only limits
//! JSON Schema cannot represent, such as byte limits or cross-field arithmetic, belong
//! in description prose. Numeric bounds must match host validation, not UI help text.
//! Description prose remains English regardless of UI locale.
//!
//! `api.rs::tool_schema` checks descriptor-provided `input_schema` (MCP and
//! `structured_output`), this module, legacy `memory_*` schemas, then generic parameter
//! derivation.

use serde_json::{json, Value};

use crate::agents::{
    MAX_WAIT_AGENT_NAMES, WAIT_DEFAULT_TIMEOUT_SECONDS, WAIT_MAX_TIMEOUT_SECONDS,
    WAIT_MIN_TIMEOUT_SECONDS,
};
use crate::prompt_profile::{PromptKey, PromptProfile};
use crate::workspace_set::WorkspaceSet;
use workflow_core::MAX_SCRIPT_BYTES;

/// The `description` parameter text shared by the shell tools.
///
/// It is worded at the model rather than at the schema on purpose: the value is
/// what a person reads in the approval card and the task row, so a description
/// that hedges ("possibly risky…") is worse than none.
const SHELL_DESCRIPTION_PARAMETER: &str = "One short sentence, in active voice, saying what this command does. Name the action itself; do not hedge with words such as \"complex\" or \"risky\".\n\nFor ordinary commands (git, npm, everyday CLI tools) keep it to five to ten words:\n- ls → \"List files in current directory\"\n- git status → \"Show working tree status\"\n- npm install → \"Install project dependencies\"\n\nFor commands that are hard to read at a glance (pipelines, unusual flags, find/xargs) add just enough context to make the effect clear:\n- find . -name \"*.tmp\" -exec rm {} \\; → \"Delete every .tmp file under the current directory\"\n- git reset --hard origin/main → \"Discard local changes and match remote main\"\n- curl -s url | jq '.data[]' → \"Fetch JSON from a URL and print its data entries\"";

/// The `timeout` parameter text, built from the constants that actually bound it
/// so the schema cannot drift from the clamp.
fn shell_timeout_parameter_description() -> String {
    format!(
        "Optional timeout in milliseconds (default {}, max {}). On expiry the command is moved to the background rather than stopped, and the receipt carries its shell:<id> address.",
        crate::tool_executor::SHELL_DEFAULT_TIMEOUT.as_millis(),
        crate::tool_executor::SHELL_MAX_TIMEOUT.as_millis()
    )
}

/// Line ceiling both preview log tools clamp to (`preview_servers::MAX_LOG_LINES`),
/// so it is also the largest `lines` that changes anything.
const PREVIEW_MAX_LOG_LINES: u32 = 200;

/// Largest emulated viewport edge in CSS pixels. The source validates 1..=9999 and
/// names the range in its own refusal.
const PREVIEW_MAX_VIEWPORT: u32 = 9999;

fn string_prop(description: &str, max_length: usize) -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": max_length,
        "description": description
    })
}

/// Which server (and therefore which page) a preview tool acts on.
///
/// Optional, deliberately, against the source schema: Claude Code marks it required
/// yet its own dispatcher documents the absent case — explicit id, else the first
/// running server for the worktree, else the session's preview page. In Mework the
/// preview page can exist with no dev server behind it, so a required parameter with
/// a documented absent case would be a lie.
fn server_id_prop() -> Value {
    string_prop("Server ID", 256)
}

/// The natural-language query every decision-model find tool scores candidates against.
fn query_prop(description: &str) -> Value {
    string_prop(description, crate::decision_model::MAX_QUERY_CHARS)
}

/// The score threshold every decision-model find tool takes: `0..=1`, three decimals. Scores
/// come back at the same precision, so a reported score can be reused as a threshold.
fn threshold_prop() -> Value {
    threshold_prop_with(SCORE_THRESHOLD_DESCRIPTION)
}

const SCORE_THRESHOLD_DESCRIPTION: &str = "Minimum score, 0 to 1 with at most three decimal places; only pieces scoring at or above it are returned. Start around 0.6 and lower it if nothing comes back.";

fn threshold_prop_with(description: &str) -> Value {
    json!({
        "type": "number",
        "minimum": 0,
        "maximum": 1,
        "description": description
    })
}

/// What the decision parameters mean on one decision-parameter tool.
struct DecisionParameterText {
    query: &'static str,
    /// `None` on the element tools: they choose one element (or none of the above) rather than
    /// score, so there is no threshold to cut at.
    threshold: Option<&'static str>,
    /// What the tool answers when a call leaves the decision parameters out, for `Augment`'s
    /// `query`.
    without: &'static str,
    /// What a call with the decision parameters does, for the note on the root description.
    does: &'static str,
}

/// Where the material goes and what it needs, said once on every moded tool the way the
/// standalone decision-model tools say it.
const DECISION_PROVIDER_NOTE: &str = "The material is sent to TypeSafe's API; needs the TypeSafe key from Settings → Decision model providers.";

/// How an element tool's choice declines, said on each of them.
const NONE_OF_THE_ABOVE: &str = "The model may also answer none of the above, which the host always offers; then nothing is done, and the result reports the closest elements and lists every element line the model was shown, so there is no need to read the page again before the next step.";

fn decision_parameter_text(tool: &str) -> DecisionParameterText {
    match tool {
        "preview_console_logs" => DecisionParameterText {
            query: "What to look for in the console, in plain language. The entries that pass level (the most recent lines of them, or all of them when lines is omitted) are sent to the TypeSafe Jev decision model, and only the pieces scoring at or above threshold come back, each with its line range.",
            threshold: Some(SCORE_THRESHOLD_DESCRIPTION),
            without: "Leave query and threshold out for the plain listing.",
            does: "the TypeSafe Jev decision model scores the entries that pass level and only the pieces at or above threshold come back",
        },
        "preview_snapshot" => DecisionParameterText {
            query: "The page element to look for, in plain language. Instead of the whole snapshot, its element lines are sent to the TypeSafe Jev decision model and only the elements scoring at or above threshold come back, each with its uid, its snapshot line and a CSS selector for preview_click, preview_fill or preview_inspect.",
            threshold: Some(SCORE_THRESHOLD_DESCRIPTION),
            without: "Leave query and threshold out for the whole snapshot.",
            does: "the TypeSafe Jev decision model scores the page's elements and only those at or above threshold come back, each with a selector",
        },
        "preview_click" => DecisionParameterText {
            query: "Plain-language description of the element to click, e.g. 'the Save button in the dialog'. The page's element lines are sent to the TypeSafe Jev decision model, which chooses the one meant; the result names it, with the model's confidence and the runners-up.",
            threshold: None,
            without: "Instead of selector.",
            does: "the TypeSafe Jev decision model chooses the element query describes, and that element is clicked",
        },
        "preview_fill" => DecisionParameterText {
            query: "Plain-language description of the input to fill, e.g. 'the email field'. The page's element lines are sent to the TypeSafe Jev decision model, which chooses the one meant; the result names it, with the model's confidence and the runners-up.",
            threshold: None,
            without: "Instead of selector.",
            does: "the TypeSafe Jev decision model chooses the input query describes, and that input is filled",
        },
        _ => DecisionParameterText {
            query: "Plain-language description of the element to inspect, e.g. 'the Save button in the dialog'. The page's element lines are sent to the TypeSafe Jev decision model, which chooses the one meant; the result names it, with the model's confidence and the runners-up.",
            threshold: None,
            without: "Instead of selector.",
            does: "the TypeSafe Jev decision model chooses the element query describes, and that element's styles are read",
        },
    }
}

/// `schema` rewritten for one decision-parameter mode of its tool
/// ([`crate::decision_tools::DECISION_PARAMETER_TOOLS`]).
///
/// `Augment` adds the tool's decision parameters ([`crate::decision_tools::decision_parameters`])
/// as optional properties and releases the direct targeting parameters from `required`, so a
/// call names its target one way or the other. `Replace` removes the direct parameters and
/// requires the decision ones. Everything else the tool takes (`value`, `styles`, `doubleClick`,
/// `level`, `lines`) is untouched.
///
/// "Exactly one of `selector` or `query`" and "`threshold` with `query`" stay prose and host
/// checks (`decision_tools::parameter_mode_rejection`, `decision_model::parse_threshold`) rather
/// than a root `oneOf`: these tools stay flat objects, which is what every provider's tool dialect
/// accepts.
pub(crate) fn with_decision_parameters(
    mut schema: Value,
    tool: &str,
    mode: crate::model::DecisionParameterMode,
) -> Value {
    use crate::model::DecisionParameterMode;

    let text = decision_parameter_text(tool);
    let direct = crate::decision_tools::direct_parameters(tool);
    let decision = crate::decision_tools::decision_parameters(tool);
    let named = decision.join(" and ");
    let Some(object) = schema.as_object_mut() else {
        return schema;
    };
    let instead = if direct.is_empty() {
        String::new()
    } else {
        format!(" instead of {}", direct.join(" and "))
    };
    if let Some(Value::String(description)) = object.get_mut("description") {
        let lead = match mode {
            DecisionParameterMode::Augment => format!("With {named}{instead}"),
            DecisionParameterMode::Replace => {
                format!("In this conversation every call takes {named}{instead}")
            }
        };
        let declines = if text.threshold.is_none() {
            format!(" {NONE_OF_THE_ABOVE}")
        } else {
            String::new()
        };
        description.push_str(&format!(
            "\n\n{lead}: {}.{declines} {DECISION_PROVIDER_NOTE}",
            text.does
        ));
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        match mode {
            DecisionParameterMode::Augment => {
                for key in direct {
                    if let Some(Value::String(description)) = properties
                        .get_mut(*key)
                        .and_then(|property| property.get_mut("description"))
                    {
                        description.push_str(&format!(". Or leave it out and give {named}."));
                    }
                }
                properties.insert(
                    "query".into(),
                    query_prop(&format!("{} {}", text.without, text.query)),
                );
                if let Some(threshold) = text.threshold {
                    properties.insert(
                        "threshold".into(),
                        threshold_prop_with(&format!("Required with query. {threshold}")),
                    );
                }
            }
            DecisionParameterMode::Replace => {
                for key in direct {
                    properties.remove(*key);
                }
                properties.insert("query".into(), query_prop(text.query));
                if let Some(threshold) = text.threshold {
                    properties.insert("threshold".into(), threshold_prop_with(threshold));
                }
            }
        }
        if tool == "preview_console_logs" {
            if let Some(lines) = properties.get_mut("lines") {
                lines["description"] = json!(match mode {
                    DecisionParameterMode::Augment => "Max lines to return (default: 50, max: 200). With query: how many of the most recent entries to score (default: every entry that passes level).",
                    DecisionParameterMode::Replace => "How many of the most recent entries to score (max: 200; default: every entry that passes level).",
                });
            }
        }
    }
    if let Some(required) = object.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|name| !name.as_str().is_some_and(|name| direct.contains(&name)));
        if mode == DecisionParameterMode::Replace {
            required.extend(decision.iter().map(|name| json!(name)));
        }
    }
    schema
}

/// One closed variant of a merged tool's `oneOf`.
///
/// `action` is folded into each variant as a `const` and into its `required`, so the union stays
/// discriminated: a model picking an action sees exactly that action's parameters, and every
/// boundary keyword of the pre-merge per-operation schemas survives unchanged.
fn action_variant(action: &str, description: &str, mut body: Value) -> Value {
    let object = body.as_object_mut().expect("variant object");
    object.insert("type".into(), json!("object"));
    object.insert("description".into(), json!(description));
    object.insert("additionalProperties".into(), json!(false));
    let properties = object
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("variant properties");
    properties.insert(
        "action".into(),
        json!({"const": action, "description": "Selects this operation."}),
    );
    let required = object
        .entry("required")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("variant required");
    required.insert(0, json!("action"));
    body
}

/// Every `todo` operation, in catalog order.
///
/// `action` is a `const` inside each variant, so the model that picks an action sees
/// exactly that action's parameters and every boundary keyword of the pre-merge
/// per-operation schemas survives — including `update`'s "at least one patch field"
/// `anyOf`.
fn todo_variants() -> Vec<Value> {
    let task_id = || json!({"type": "string", "minLength": 1, "maxLength": 128});
    let relation_ids = |description: &str| {
        json!({
            "type": "array",
            "maxItems": 256,
            "uniqueItems": true,
            "items": {"type": "string", "minLength": 1, "maxLength": 128},
            "description": description
        })
    };
    vec![
        action_variant(
            "create",
            "Create one pending task in this conversation's task list and return its stable task ID.",
            json!({
                "properties": {
                    "subject": {"type": "string", "minLength": 1, "maxLength": 500},
                    "description": {"type": "string", "minLength": 1, "maxLength": 32768},
                    "activeForm": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 500,
                        "description": "Present-continuous text shown while the task is in_progress."
                    },
                    "metadata": {"type": "object", "maxProperties": 256, "additionalProperties": true}
                },
                "required": ["subject", "description"]
            }),
        ),
        action_variant(
            "update",
            "Patch one existing task by taskId: status, details, dependencies or metadata. status=deleted removes the task.",
            json!({
                "properties": {
                    "taskId": task_id(),
                    "status": {
                        "type": "string",
                        "enum": ["pending", "in_progress", "completed", "deleted"]
                    },
                    "subject": {"type": "string", "minLength": 1, "maxLength": 500},
                    "description": {"type": "string", "minLength": 1, "maxLength": 32768},
                    "activeForm": {"type": "string", "minLength": 1, "maxLength": 500},
                    "addBlocks": relation_ids("Task IDs this task blocks."),
                    "addBlockedBy": relation_ids("Task IDs that block this task."),
                    "owner": {"type": "string", "minLength": 1, "maxLength": 256},
                    "metadata": {
                        "type": "object",
                        "maxProperties": 256,
                        "additionalProperties": true,
                        "description": "Keys merge into the task's metadata; a null value deletes that key."
                    }
                },
                "required": ["taskId"],
                "anyOf": [
                    {"required": ["status"]},
                    {"required": ["subject"]},
                    {"required": ["description"]},
                    {"required": ["activeForm"]},
                    {"required": ["addBlocks"]},
                    {"required": ["addBlockedBy"]},
                    {"required": ["owner"]},
                    {"required": ["metadata"]}
                ]
            }),
        ),
        action_variant(
            "get",
            "Read one task's full details, status and dependencies without changing the list.",
            json!({
                "properties": {"taskId": task_id()},
                "required": ["taskId"]
            }),
        ),
        action_variant(
            "list",
            "List every task in the task list with status, owner and unresolved dependencies.",
            json!({"properties": {}}),
        ),
    ]
}

pub(crate) fn builtin_tool_schema(name: &str, profile: &PromptProfile) -> Option<Value> {
    let schema = match name {
        // ---------------------------------------------------------------- Files
        "ls" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolLsDescription),
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "default": ".",
                    "description": "Directory to list, relative to the workspace."
                },
                "depth": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 8,
                    "default": 1,
                    "description": "Recursion depth; 0 lists only the directory itself."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "grep" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolGrepDescription),
            "properties": {
                "pattern": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "description": "Regular expression to match against each line."
                },
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "default": ".",
                    "description": "File or directory to search, relative to the workspace."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Match case-sensitively."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
        "find" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolFindDescription),
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 1024,
                    "description": "Glob pattern matched against relative paths and basenames."
                },
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "default": ".",
                    "description": "Directory to search, relative to the workspace."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        "read" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolReadDescription),
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "description": "File to read, relative to the workspace."
                },
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "default": 1,
                    "description": "First line to return, 1-based. Text files only."
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line to return, inclusive; must not be smaller than start_line. Text files only."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "find_content" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolFindContentDescription),
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "description": "Text file to search, relative to the workspace."
                },
                "query": query_prop("What to look for, in plain language, e.g. 'the code that handles a failed login'."),
                "threshold": threshold_prop(),
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "default": 1,
                    "description": "First line to consider, 1-based."
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line to consider, inclusive; defaults to the end of the file."
                }
            },
            "required": ["path", "query", "threshold"],
            "additionalProperties": false
        }),
        "find_files" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolFindFilesDescription),
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "default": ".",
                    "description": "Directory to search, relative to the workspace."
                },
                "query": query_prop("What files to look for, in plain language, e.g. 'code that stores provider credentials'."),
                "threshold": threshold_prop(),
                "depth": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 8,
                    "default": 6,
                    "description": "Recursion depth; 0 lists only the current directory."
                }
            },
            "required": ["query", "threshold"],
            "additionalProperties": false
        }),
        "find_output" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolFindOutputDescription),
            "properties": {
                "task": string_prop("The shell task address from task_list, e.g. shell:3.", 64),
                "query": query_prop("What to look for in the command output, in plain language."),
                "threshold": threshold_prop()
            },
            "required": ["task", "query", "threshold"],
            "additionalProperties": false
        }),
        "lsp" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolLspDescription),
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": [
                        "goToDefinition",
                        "findReferences",
                        "hover",
                        "documentSymbol",
                        "workspaceSymbol",
                        "goToImplementation",
                        "prepareCallHierarchy",
                        "incomingCalls",
                        "outgoingCalls"
                    ],
                    "description": "The LSP operation to perform"
                },
                "filePath": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "description": "The absolute or relative path to the file"
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The line number (1-based, as shown in editors)"
                },
                "character": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The character offset (1-based, as shown in editors)"
                },
                "query": {
                    "type": "string",
                    "maxLength": 1024,
                    "description": "The symbol name or partial name to search for (workspaceSymbol only). Most language servers return no results for an empty query, so always provide it when using workspaceSymbol."
                }
            },
            "required": ["operation", "filePath", "line", "character"],
            "additionalProperties": false
        }),
        "write" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolWriteDescription),
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "description": "Target file, relative to the workspace."
                },
                "content": {
                    "type": "string",
                    "description": "The complete new file content; an empty string is allowed."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
        "edit" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolEditDescription),
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "description": "Existing file to modify, relative to the workspace."
                },
                "find": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Exact text to replace; must match exactly once."
                },
                "replace": {
                    "type": "string",
                    "description": "Replacement text; an empty string deletes the passage."
                }
            },
            "required": ["path", "find", "replace"],
            "additionalProperties": false
        }),
        // ---------------------------------------------------------------- Commands
        "powershell" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPowershellDescription),
            "properties": {
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 65536,
                    "description": "The PowerShell command line."
                },
                "description": {
                    "type": "string",
                    "description": SHELL_DESCRIPTION_PARAMETER
                },
                "timeout": {
                    "type": "number",
                    "description": shell_timeout_parameter_description()
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Run the command as a background task instead of blocking this call. The receipt carries its shell:<id> address."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
        "bash" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolBashDescription),
            "properties": {
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 65536,
                    "description": "The Bash command line."
                },
                "description": {
                    "type": "string",
                    "description": SHELL_DESCRIPTION_PARAMETER
                },
                "timeout": {
                    "type": "number",
                    "description": shell_timeout_parameter_description()
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Run the command as a background task instead of blocking this call. The receipt carries its shell:<id> address."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
        "bash_find_output" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolBashFindOutputDescription),
            "properties": {
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 65536,
                    "description": "The Bash command line."
                },
                "description": {
                    "type": "string",
                    "description": SHELL_DESCRIPTION_PARAMETER
                },
                "timeout": {
                    "type": "number",
                    "description": shell_timeout_parameter_description()
                },
                "query": query_prop("What to look for in the command output, in plain language."),
                "threshold": threshold_prop()
            },
            "required": ["command", "query", "threshold"],
            "additionalProperties": false
        }),
        "zsh" => shell_command_schema(profile.text(PromptKey::ToolZshDescription), "The zsh command line."),
        "sh" => shell_command_schema(profile.text(PromptKey::ToolShDescription), "The POSIX sh command line."),
        "zsh_find_output" => shell_find_output_schema(
            profile.text(PromptKey::ToolZshFindOutputDescription),
            "The zsh command line.",
        ),
        "sh_find_output" => shell_find_output_schema(
            profile.text(PromptKey::ToolShFindOutputDescription),
            "The POSIX sh command line.",
        ),
        "powershell_find_output" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPowershellFindOutputDescription),
            "properties": {
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 65536,
                    "description": "The PowerShell command line."
                },
                "description": {
                    "type": "string",
                    "description": SHELL_DESCRIPTION_PARAMETER
                },
                "timeout": {
                    "type": "number",
                    "description": shell_timeout_parameter_description()
                },
                "query": query_prop("What to look for in the command output, in plain language."),
                "threshold": threshold_prop()
            },
            "required": ["command", "query", "threshold"],
            "additionalProperties": false
        }),

        //
        // The schemas mirror Cherry Studio's `shared/ai/builtinTools.ts`: `web_search`
        // accepts one self-contained query, `web_fetch` accepts absolute URLs, and both
        // return a result list with per-call IDs usable as `[cite:id]` references.
        "web_search" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolWebSearchDescription),
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": crate::api::WEB_SEARCH_MIN_QUERY,
                    "maxLength": crate::api::WEB_SEARCH_MAX_QUERY,
                    "description": "Self-contained search query. MUST NOT use pronouns or context-dependent references; expand the topic from earlier messages when the user asks a follow-up. Break a long question into several searches rather than one long sentence."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        "web_fetch" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolWebFetchDescription),
            "properties": {
                "urls": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": crate::model::MAX_SEARCH_INPUTS,
                    "items": {
                        "type": "string",
                        "minLength": 1,
                        // Do not use `format: "uri"`: strict OpenAI-compatible upstreams
                        // reject the whole request. The host validates absolute http(s) URLs.
                        "description": "An absolute http(s) page URL."
                    },
                    "description": "Absolute http(s) page URLs to fetch. Use web_search first when you do not know the URL."
                }
            },
            "required": ["urls"],
            "additionalProperties": false
        }),
        // ------------------------------------------------------------- Preview
        //
        // Thirteen of these are Claude Code's preview tools, schemas and prose
        // included; `preview_upload_image` and `preview_dialog` are Mework's own,
        // because the source has no equivalent and dropping them would drop the
        // feature. Parameter descriptions are the source's words verbatim, including
        // `scale`'s closing sentence about `preview_click` — with `.mework/launch.json`
        // standing in for the source's `.claude/launch.json`, whose path belongs to a
        // product that may be installed alongside this one.
        "preview_start" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewStartDescription),
            "properties": {
                "name": string_prop("Server name from .mework/launch.json.", 256)
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        "preview_stop" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewStopDescription),
            "properties": {
                "serverId": string_prop("Server ID to stop", 256)
            },
            "required": ["serverId"],
            "additionalProperties": false
        }),
        "preview_list" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewListDescription),
            "properties": {},
            "additionalProperties": false
        }),
        "preview_logs" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewLogsDescription),
            "properties": {
                "serverId": server_id_prop(),
                "level": {
                    "type": "string",
                    "enum": ["all", "error"],
                    "description": "Filter by level: 'all' (default) shows all output, 'error' shows only lines containing error/exception/failed/fatal"
                },
                "lines": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": PREVIEW_MAX_LOG_LINES,
                    "description": "Max lines to return (default: 50)"
                },
                "search": string_prop(
                    "Filter to lines containing this text (e.g., '[DEBUG]', 'POST /api')",
                    512
                )
            },
            "required": [],
            "additionalProperties": false
        }),
        "preview_console_logs" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewConsoleLogsDescription),
            "properties": {
                "serverId": server_id_prop(),
                "level": {
                    "type": "string",
                    "enum": ["all", "error", "warn"],
                    "description": "Filter by level: 'all' (default), 'error' (errors only), 'warn' (warnings + errors)"
                },
                "lines": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": PREVIEW_MAX_LOG_LINES,
                    "description": "Max lines to return (default: 50, max: 200)"
                }
            },
            "required": [],
            "additionalProperties": false
        }),
        "preview_screenshot" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewScreenshotDescription),
            "properties": {
                "serverId": server_id_prop(),
                "scale": {
                    "type": "number",
                    "minimum": 0.1,
                    "maximum": 1,
                    "description": "Scale factor in [0.1, 1] for the returned image; smaller images use fewer tokens. preview_click uses element UIDs from preview_snapshot, not pixel coordinates."
                }
            },
            "required": [],
            "additionalProperties": false
        }),
        "preview_snapshot" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewSnapshotDescription),
            "properties": {
                "serverId": server_id_prop()
            },
            "required": [],
            "additionalProperties": false
        }),
        "preview_inspect" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewInspectDescription),
            "properties": {
                "serverId": server_id_prop(),
                "selector": string_prop("CSS selector (e.g., '.button', '#header')", 2048),
                "styles": {
                    "type": "array",
                    "maxItems": 64,
                    "items": {"type": "string", "minLength": 1, "maxLength": 128},
                    "description": "CSS properties to return (e.g., ['padding', 'color']). Defaults to common properties."
                }
            },
            "required": ["selector"],
            "additionalProperties": false
        }),
        "preview_click" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewClickDescription),
            "properties": {
                "serverId": server_id_prop(),
                "selector": string_prop("CSS selector for the element to click", 2048),
                "doubleClick": {
                    "type": "boolean",
                    "description": "Perform a double-click"
                }
            },
            "required": ["selector"],
            "additionalProperties": false
        }),
        "preview_fill" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewFillDescription),
            "properties": {
                "serverId": server_id_prop(),
                "selector": string_prop("CSS selector for the input element", 2048),
                // No `minLength`: filling with the empty string is how a field is cleared.
                "value": {
                    "type": "string",
                    "maxLength": 32768,
                    "description": "Value to fill"
                }
            },
            "required": ["selector", "value"],
            "additionalProperties": false
        }),
        "preview_eval" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewEvalDescription),
            "properties": {
                "serverId": server_id_prop(),
                "expression": string_prop(
                    "JavaScript expression to evaluate in the page context. Return values are serialized as JSON.",
                    65536
                )
            },
            "required": ["expression"],
            "additionalProperties": false
        }),
        "preview_network" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewNetworkDescription),
            "properties": {
                "serverId": server_id_prop(),
                "filter": {
                    "type": "string",
                    "enum": ["all", "failed"],
                    "description": "Filter: 'all' (default) shows all requests, 'failed' shows only 4xx/5xx and network errors. Ignored when requestId is provided."
                },
                "requestId": string_prop(
                    "If provided, returns the response body for this specific request instead of listing all requests. Get requestIds from the listing output.",
                    256
                )
            },
            "required": [],
            "additionalProperties": false
        }),
        "preview_resize" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewResizeDescription),
            "properties": {
                "serverId": server_id_prop(),
                "preset": {
                    "type": "string",
                    "enum": ["mobile", "tablet", "desktop"],
                    "description": "Device preset. Overrides width/height if provided. \"desktop\" clears the size emulation (back to the pane's responsive size)."
                },
                "width": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": PREVIEW_MAX_VIEWPORT,
                    "description": "Viewport width in CSS pixels (requires height)"
                },
                "height": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": PREVIEW_MAX_VIEWPORT,
                    "description": "Viewport height in CSS pixels (requires width)"
                },
                "colorScheme": {
                    "type": "string",
                    "enum": ["light", "dark"],
                    "description": "Emulate prefers-color-scheme media feature for dark/light mode testing."
                }
            },
            "required": [],
            "additionalProperties": false
        }),
        "preview_upload_image" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewUploadImageDescription),
            "properties": {
                "serverId": server_id_prop(),
                "image_id": string_prop(
                    "The conversation image number, e.g. 3, #3, or [Image #3]. A 64-character hex digest also resolves.",
                    128
                ),
                "selector": string_prop(
                    "CSS selector of the target file input. Omitted, the first file input on the page is used.",
                    2048
                ),
                "filename": string_prop(
                    "The file name the page sees. Defaults to the attachment's own name.",
                    256
                )
            },
            "required": ["image_id"],
            "additionalProperties": false
        }),
        "preview_dialog" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewDialogDescription),
            "properties": {
                "serverId": server_id_prop(),
                "accept": {
                    "type": "boolean",
                    "description": "true accepts the open dialog, false dismisses it (default true)."
                },
                // No `minLength`: an empty answer is what a prompt dialog's own default is.
                "prompt_text": {
                    "type": "string",
                    "maxLength": 4096,
                    "description": "The answer for an open prompt dialog, used only when accepting."
                }
            },
            "required": [],
            "additionalProperties": false
        }),
        "preview_find_logs" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPreviewFindLogsDescription),
            "properties": {
                "serverId": server_id_prop(),
                "query": query_prop("What to look for in the console and server logs, in plain language."),
                "threshold": threshold_prop(),
                "source": {
                    "type": "string",
                    "enum": ["all", "console", "server"],
                    "default": "all",
                    "description": "Which logs to search: all, console, or server."
                }
            },
            "required": ["query", "threshold"],
            "additionalProperties": false
        }),
        "agent_spawn" => agent_spawn_schema(None, false, profile),
        "send_message" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolSendMessageDescription),
            "properties": {
                "target": {
                    "type": "string",
                    "pattern": "^[a-z][a-z0-9_-]{0,31}$",
                    "description": "Child agent name returned by agent_spawn."
                },
                "message": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 32768,
                    "description": "Message text."
                }
            },
            "required": ["target", "message"],
            "additionalProperties": false
        }),
        "followup_task" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolFollowupTaskDescription),
            "properties": {
                "target": {
                    "type": "string",
                    "pattern": "^[a-z][a-z0-9_-]{0,31}$",
                    "description": "Child agent name returned by agent_spawn."
                },
                "message": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 32768,
                    "description": "Instruction text."
                }
            },
            "required": ["target", "message"],
            "additionalProperties": false
        }),
        "task_wait" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolTaskWaitDescription),
            "properties": {
                "tasks": {
                    "type": "array",
                    "maxItems": MAX_WAIT_AGENT_NAMES,
                    "items": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 320,
                        "description": "A child agent name, a workflow run name (also accepted as workflow:<name>), shell:<id>, or terminal:<id>."
                    },
                    "description": "Task addresses to wait on; omitted waits for every child agent, workflow run and background shell command in this conversation (terminals excluded)."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": WAIT_MIN_TIMEOUT_SECONDS,
                    "maximum": WAIT_MAX_TIMEOUT_SECONDS,
                    "default": WAIT_DEFAULT_TIMEOUT_SECONDS,
                    "description": "Wait deadline in seconds. Set it to match how long the work should take — a child agent's turn can run for many minutes. Reaching the deadline is not a failure: the tasks keep running and nothing is lost. Wait again, or end the round and the host delivers the result on its own."
                }
            },
            "required": [],
            "additionalProperties": false
        }),
        "task_list" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolTaskListDescription),
            "properties": {},
            "additionalProperties": false
        }),
        "box" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolBoxDescription),
            "properties": {},
            "additionalProperties": false
        }),
        // ---------------------------------------------------------- Long-term memory
        "read_global_memory" => {
            memory_read_schema(profile.text(PromptKey::ToolReadGlobalMemoryDescription))
        }
        "read_project_memory" => {
            memory_read_schema(profile.text(PromptKey::ToolReadProjectMemoryDescription))
        }
        "create_global_memory" => {
            memory_create_schema(profile.text(PromptKey::ToolCreateGlobalMemoryDescription))
        }
        "create_project_memory" => {
            memory_create_schema(profile.text(PromptKey::ToolCreateProjectMemoryDescription))
        }
        "edit_global_memory" => {
            memory_edit_schema(profile.text(PromptKey::ToolEditGlobalMemoryDescription))
        }
        "edit_project_memory" => {
            memory_edit_schema(profile.text(PromptKey::ToolEditProjectMemoryDescription))
        }
        // ------------------------------------------------------------ User interaction
        "ask_user" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolAskUserDescription),
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": 4000,
                                "description": "The question shown to the user."
                            },
                            "header": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": 12,
                                "description": "Chip/tag label for the question."
                            },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 4,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "minLength": 1,
                                            "maxLength": 120,
                                            "description": "Display text of this option."
                                        },
                                        "description": {
                                            "type": "string",
                                            "minLength": 1,
                                            "maxLength": 1000,
                                            "description": "What choosing this option means."
                                        },
                                        "preview": {
                                            "type": "string",
                                            "maxLength": 16384,
                                            "description": "Preview content rendered while this option is focused."
                                        }
                                    },
                                    "required": ["label", "description"],
                                    "additionalProperties": false
                                }
                            },
                            "multiSelect": {
                                "type": "boolean",
                                "description": "Allow selecting multiple options."
                            }
                        },
                        "required": ["question", "header", "options", "multiSelect"],
                        "additionalProperties": false
                    }
                },
                "answers": {
                    "type": "object",
                    "description": "User answers collected by the permission component.",
                    "additionalProperties": {"type": "string"}
                },
                "annotations": {
                    "type": "object",
                    "description": "Per-question annotations from the user."
                },
                "metadata": {
                    "type": "object",
                    "description": "Tracking metadata; not shown to the user."
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        }),
        "fork" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolForkDescription),
            "properties": {
                "prompt": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 32768,
                    "description": "First user message of the forked conversation, and the only instruction you will ever give it — there is no channel for a correction afterwards. State the task and every piece of background it needs: the child sees nothing else."
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        }),
        // ------------------------------------------------------------ Task state
        "todo" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolTodoDescription),
            "oneOf": todo_variants()
        }),
        // ------------------------------------------------------------ Plan mode
        // Flat rather than an action `oneOf`: the Claude Agent provider publishes
        // host tools through MCP, and the plan must be writable there.
        "plan" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPlanDescription),
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["write", "read"],
                    "description": "`write` stores or replaces this conversation's plan document with `content`; `read` returns the document currently stored."
                },
                "content": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 200000,
                    "description": "Required for `write`. The plan's Markdown body: context, the recommended approach, the critical files, the utilities to reuse, and how the work will be verified. The whole document is replaced, so send the complete plan every time."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        }),
        // A request, not a payload: the plan it acts on is the stored document,
        // and the answer is the user's.
        "exit_plan_mode" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolExitPlanModeDescription),
            "properties": {},
            "additionalProperties": false
        }),
        // ------------------------------------------------------------ Workflow
        // The static baseline is the permissive form used for golden-file and catalog
        // comparisons; host-generated per-run schemas apply any narrowing.
        "workflow" => workflow_schema(None, false, profile),
        // ------------------------------------------------------------ Skills
        // The static baseline has no `enum`: no available skills and unknown available
        // skills are different claims, and unavailable documentation yields this schema.
        "skill" => skill_schema(profile),
        "tool_search" => tool_search_schema(profile),
        _ => return None,
    };
    Some(schema)
}

/// The schema of a shell backend's command tool: the shape `bash` and
/// `powershell` spell out in full, for the backends added after them.
fn shell_command_schema(description: &str, command_description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "command": {
                "type": "string",
                "minLength": 1,
                "maxLength": 65536,
                "description": command_description
            },
            "description": {
                "type": "string",
                "description": SHELL_DESCRIPTION_PARAMETER
            },
            "timeout": {
                "type": "number",
                "description": shell_timeout_parameter_description()
            },
            "run_in_background": {
                "type": "boolean",
                "description": "Run the command as a background task instead of blocking this call. The receipt carries its shell:<id> address."
            }
        },
        "required": ["command"],
        "additionalProperties": false
    })
}

/// The schema of a shell backend's scoring tool, as [`shell_command_schema`]
/// is to its command tool.
fn shell_find_output_schema(description: &str, command_description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "command": {
                "type": "string",
                "minLength": 1,
                "maxLength": 65536,
                "description": command_description
            },
            "description": {
                "type": "string",
                "description": SHELL_DESCRIPTION_PARAMETER
            },
            "timeout": {
                "type": "number",
                "description": shell_timeout_parameter_description()
            },
            "query": query_prop("What to look for in the command output, in plain language."),
            "threshold": threshold_prop()
        },
        "required": ["command", "query", "threshold"],
        "additionalProperties": false
    })
}

/// Whether a tool's arguments name a place — a path to act on, or a command
/// whose working directory and machine follow from where it runs.
///
/// `preview_start` is the one preview tool that does: which workspace's
/// `.mework/launch.json` it reads decides where the server runs — on that
/// workspace's machine, an SSH machine included. The rest of the preview,
/// browser and memory tools are absent on purpose: they act on a server by its
/// id, on a page, or on a Markdown store the host owns, and a workspace number
/// would advertise a choice that changes nothing.
pub(crate) fn takes_a_workspace(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "ls" | "grep"
            | "find"
            | "read"
            | "write"
            | "edit"
            | "lsp"
            | "find_content"
            | "find_files"
            | "preview_start"
    ) || crate::shell_backend::ShellBackend::of_tool(tool_name).is_some()
}

/// Adds the `workspace` parameter naming which workspace a call acts in.
///
/// The parameter appears only when the conversation has more than one workspace.
/// With a single workspace there is nothing to choose: every call lands there,
/// and an enum of one value would be a question whose answer is already known —
/// tokens spent, and one more thing for a model to get wrong.
///
/// `enum` rather than a free integer because the allowed set is per tool. A
/// shell tool runs in one backend, and a machine has only the backends its
/// probe found, so each shell tool lists only the workspaces on machines that
/// have its shell; a `zsh` call naming a Windows workspace is not a call the
/// host could honour, and refusing it in the schema is cheaper than refusing it
/// in a tool result.
pub(crate) fn with_workspace_parameter(
    mut schema: Value,
    tool_name: &str,
    workspaces: &WorkspaceSet,
) -> Value {
    if workspaces.len() < 2 || !takes_a_workspace(tool_name) {
        return schema;
    }
    let backend = crate::shell_backend::ShellBackend::of_tool(tool_name);
    let addresses = match backend {
        Some(backend) => workspaces.shell_addresses(backend),
        None => workspaces.addresses(),
    };
    // No address means the tool has nowhere to run at all, which withdraws it
    // from the request entirely; an empty enum here would be an unsatisfiable
    // schema on a tool the model can still see.
    let Some(default) = addresses.first().copied() else {
        return schema;
    };
    let mut description = format!(
        "Which of this conversation's workspaces this call acts in, named by the number the Environment section gives it. Defaults to {default}."
    );
    if let Some(backend) = backend.filter(|_| addresses.len() < workspaces.len()) {
        description.push_str(&format!(
            " Only workspaces whose machine has {} are listed; use another shell tool for the others.",
            backend.display_name()
        ));
    }
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    else {
        return schema;
    };
    properties.insert(
        "workspace".to_owned(),
        json!({
            "type": "integer",
            "enum": addresses,
            "default": default,
            "description": description
        }),
    );
    schema
}

fn memory_document_name(description: &str) -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 120,
        "pattern": "^[^/\\\\]+$",
        "description": description
    })
}

fn memory_read_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "name": memory_document_name(
                "Document name from the memory index; the .md suffix is optional."
            )
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

fn memory_create_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "name": memory_document_name(
                "New document name inside the memory directory; no path separators, the .md suffix is optional."
            ),
            "content": {
                "type": "string",
                "minLength": 1,
                "description": "The document's complete Markdown body, up to 256 KiB of UTF-8."
            },
            "description": {
                "type": "string",
                "minLength": 1,
                "maxLength": 300,
                "description": "One-sentence index entry written into MEMORY.md."
            }
        },
        "required": ["name", "content", "description"],
        "additionalProperties": false
    })
}

fn memory_edit_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "name": memory_document_name(
                "Existing document name; the .md suffix is optional."
            ),
            "old_text": {
                "type": "string",
                "minLength": 1,
                "description": "Passage to replace; must occur exactly once in the document."
            },
            "new_text": {
                "type": "string",
                "description": "Replacement text; an empty string deletes the passage."
            },
            "description": {
                "type": "string",
                "minLength": 1,
                "maxLength": 300,
                "description": "One-sentence index entry describing the document after the change."
            }
        },
        "required": ["name", "old_text", "new_text", "description"],
        "additionalProperties": false
    })
}

/// Model-visible `workflow` schema: script body and optional parameters.
///
/// `roles` holds the named agents available for this run. `None` yields the static
/// baseline; `Some` yields a host-generated run-specific schema.
///
/// `role_required` controls whether every step must name a role. Because `agentType`
/// occurs inside JavaScript source, JSON Schema cannot enforce it; the prose must state
/// that violations throw synchronously in `workflow-script::issue_step`.
///
/// Step subagents inherit no conversation history, so the script API requires complete
/// behavioral descriptions. Limits come directly from `workflow-core` constants.
///
/// Role names use `$defs` as machine-readable legal values. It remains intentionally
/// unreferenced because the Responses adapter sends `strict: false`.
fn workflow_schema(
    roles: Option<&[String]>,
    role_required: bool,
    profile: &PromptProfile,
) -> Value {
    let mut schema = json!({
        "type": "object",
        "description": profile.text(PromptKey::ToolWorkflowDescription),
        "properties": {
            "script": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_SCRIPT_BYTES,
                "description": workflow_script_description(roles, role_required)
            },
            "name": {
                "type": "string",
                "pattern": "^[a-z][a-z0-9_-]{0,31}$",
                "description": "Required. Name this run yourself: the name is this run's id and its address, in the same namespace agents are named in, and the title the task is listed under. Say what the run is for (review-sweep, migrate-callsites). A name is reserved for the whole conversation branch tree; reuse one and this run is numbered instead (review-sweep-2), and the receipt reports the id it got. A resume still needs a name of its own."
            },
            "args": {
                "description": "JSON value exposed to the script as the global `args`. Pass arrays and objects directly (at most 4,096 items per array), not as encoded strings."
            },
            "token_budget": {
                "type": "integer",
                "minimum": 1,
                "description": "Optional hard token ceiling for this run, surfaced to the script as budget.total. Once step usage reaches it, further agent() calls throw."
            },
            "resume_run_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 128,
                "pattern": "^[A-Za-z0-9_-]+$",
                "description": "Run id of a previous run of this same script: the name you gave it, or the id its dispatch receipt reported when the host had to number it. Journaled steps replay instantly; the first unjournaled step and everything after it re-runs. On resume, script may be omitted — the host reloads the approved script from the run directory; if provided, it must be byte-identical to the approved one."
            }
        },
        "required": ["name"],
        "additionalProperties": false,
    });
    if let Some(names) = roles.filter(|names| !names.is_empty()) {
        schema["$defs"] = json!({
            "agentType": {
                "description": "Legal values for the agentType option of agent() inside the script. A role name is the whole model-facing surface; which provider and model it runs on is the user's configuration.",
                "enum": names
            }
        });
    }
    schema
}

/// Build the `workflow.script` description. Only the role clause varies by `roles`
/// and `role_required`.
///
/// Required-role prose must state that invalid `agentType` values throw synchronously,
/// because the value appears in JavaScript source beyond JSON Schema enforcement.
fn workflow_script_description(roles: Option<&[String]>, role_required: bool) -> String {
    let agent_type_clause = match (roles, role_required) {
        // The static baseline has no run-specific `$defs`.
        (None, _) => "agentType (a configured role name; when this conversation has roles configured, the schema carries their legal values under $defs.agentType)",
        // With no legal values, `agentType` remains optional.
        (Some([]), _) => "agentType (this conversation has no named agent configured, so this option has no legal value)",
        (Some(_), false) => "agentType (one of the names under $defs.agentType below — a bare model is rejected, because a role name is the whole model-facing surface and which provider/model it runs on is the user's configuration)",
        (Some(_), true) => "agentType (REQUIRED on every agent() call — one of the names under $defs.agentType below. A bare model is rejected: a role name is the whole model-facing surface, and which provider/model it runs on is the user's configuration. Omitting it, or naming a value outside that list, throws synchronously at the agent() call and fails the whole script — it is NOT a step that resolves to null)",
    };
    let signature = if role_required {
        "- agent(prompt, opts) -> Promise<any>"
    } else {
        "- agent(prompt, opts?) -> Promise<any>"
    };
    format!(
        concat!(
            "Plain JavaScript (not TypeScript), starting with `export const meta = {{ name, description, phases?: [{{title, detail?}}] }}` — a pure literal. The body runs as an async function: top-level await and return work, and the return value becomes the workflow result.\n",
            "Available globals:\n",
            "{signature}: spawn one step subagent. It inherits no conversation history — the prompt must be self-contained. opts: label (display name), phase (progress group; defaults to the last phase() call), schema (JSON Schema the step must satisfy; the promise then resolves to validated structured data, otherwise to the step's final text), effort (low|medium|high|xhigh), {agent_type_clause}, isolation. A failed or skipped step resolves to null.\n",
            "- isolation: \"worktree\" gives that one step its own git worktree, checked out from HEAD on a fresh branch, so parallel steps can edit files without colliding. It sees the committed tree only — your uncommitted changes are NOT in it. A step that leaves changes or commits keeps its worktree and reports the path and branch; one that changes nothing has it removed. Requires the workspace to be a git repository root; the step fails on its own if it is not. EXPENSIVE (a full checkout per step) — use it only when steps really would conflict.\n",
            "- parallel(thunks) -> Promise<any[]>: run () => agent(...) thunks concurrently and wait for all; a throwing thunk yields null. This is a barrier — use it only when the next stage needs every result.\n",
            "- pipeline(items, ...stages) -> Promise<any[]>: stream each item through the stages independently with no barrier between stages; stage callbacks receive (prev, originalItem, index), and a throwing stage drops that item to null. Default to pipeline over parallel.\n",
            "- phase(title): start a progress group; declare titles in meta.phases to pin their order. log(message): emit one narration line to the progress card.\n",
            "- args: the args input, verbatim. budget: {{ total, spent(), remaining() }} for the token_budget cap; once exhausted, further agent() calls throw.\n",
            "Date.now(), argless new Date() and Math.random() throw — they would break resume replay; pass timestamps and seeds in via args. No filesystem, network, module or timer access. At most 1000 steps per run and 4096 items per boundary array.
",
            "Required on a fresh run. Optional when resume_run_id is set — the host reloads the approved script from that run's directory."
        ),
        signature = signature,
        agent_type_clause = agent_type_clause
    )
}

/// Model-visible `agent_spawn` schema. `roles` has the same meaning as in
/// [`workflow_schema`].
///
/// When roles exist, `agent_type` uses an `enum`; when none exist, remove both the
/// unusable property and its `not` guard.
///
/// When `role_required`, `agent_type` is required and `context` is removed because
/// `context: "conversation"` and a named role are mutually exclusive across the host
/// validation layers. Fallback mode retains conversation context.
fn agent_spawn_schema(
    roles: Option<&[String]>,
    role_required: bool,
    profile: &PromptProfile,
) -> Value {
    let mut schema = json!({
        "type": "object",
        "description": profile.text(PromptKey::ToolAgentSpawnDescription),
        "properties": {
            "prompt": {
                "type": "string",
                "minLength": 1,
                "maxLength": 32768,
                "description": "The child's entire task; it sees nothing else of this conversation by default."
            },
            "agent_type": {
                "type": "string",
                "minLength": 1,
                "maxLength": 64,
                "description": "Name of a host-resolved trusted agent definition. The schema you actually receive lists this conversation's names as an enum here. The definition's prompt, model and memory identity are not model-writable."
            },
            "name": {
                "type": "string",
                "pattern": "^[a-z][a-z0-9_-]{0,31}$",
                "description": "Required. Name this child yourself: it is both the address send_message, followup_task and task_wait take, and the title the task is listed under. Say what the child is for (researcher, review-api), not what you are asking it right now. The name is reserved for the whole conversation branch tree, so it must not repeat one already used here."
            },
            "label": {
                "type": "string",
                "minLength": 1,
                "maxLength": 80,
                "description": "Short display name shown on the timeline."
            },
            "context": {
                "type": "string",
                "enum": ["none", "conversation"],
                "default": "none",
                "description": "none: the child sees only the task. conversation: a filtered copy of this conversation's history is attached."
            },
            "schema": {
                "type": "object",
                "description": "JSON Schema subset the child must satisfy via structured_output; the validated value returns with task_wait. Top level must be an object schema; supported keywords: type, properties, required, items, enum, const, additionalProperties, minItems/maxItems, minLength/maxLength, minimum/maximum. Others are rejected."
            }
        },
        "required": ["prompt", "name"],
        "not": {
            "allOf": [
                {"required": ["agent_type"]},
                {"required": ["context"], "properties": {"context": {"const": "conversation"}}}
            ]
        },
        "additionalProperties": false
    });
    match roles {
        None => {}
        Some([]) => {
            schema["properties"]
                .as_object_mut()
                .expect("agent_spawn properties object")
                .remove("agent_type");
            // The guard references `agent_type`; remove it with the absent property.
            schema
                .as_object_mut()
                .expect("agent_spawn schema object")
                .remove("not");
        }
        Some(names) => {
            schema["properties"]["agent_type"] = json!({
                "type": "string",
                "enum": names,
                "description": "Name of a host-resolved trusted agent definition. A role name is the whole model-facing surface; which provider and model it runs on is the user's configuration, and the definition's prompt and memory identity are not model-writable."
            });
            if role_required {
                schema["required"] = json!(["prompt", "name", "agent_type"]);
                // Required `agent_type` makes `context: "conversation"` impossible, so
                // remove both `context` and its dedicated mutual-exclusion guard.
                schema["properties"]
                    .as_object_mut()
                    .expect("agent_spawn properties object")
                    .remove("context");
                schema
                    .as_object_mut()
                    .expect("agent_spawn schema object")
                    .remove("not");
            }
        }
    }
    schema
}

/// Run-specific `agent_spawn` schema. See [`agent_spawn_schema`].
pub(crate) fn agent_spawn_schema_for_roles(
    names: &[String],
    role_required: bool,
    profile: &PromptProfile,
) -> Value {
    agent_spawn_schema(Some(names), role_required, profile)
}

/// Run-specific `workflow` schema. See [`workflow_schema`].
pub(crate) fn workflow_schema_for_roles(
    names: &[String],
    role_required: bool,
    profile: &PromptProfile,
) -> Value {
    workflow_schema(Some(names), role_required, profile)
}

/// Model-visible name and user-provided description of an available role.
///
/// Names are represented separately as schema `enum` or `$defs` values. The host must
/// use the summary of the highest-priority applicable definition so a shadowed role's
/// description is never advertised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentRoleSummary {
    pub name: String,
    pub description: String,
}

/// Append role descriptions to a completed tool schema's top-level `description`.
///
/// Built-in tool prose resides in this field, while `ToolDescriptor.description` is
/// reserved for user overrides. Omit roles with blank descriptions and omit the block
/// entirely when no entries remain. Preserve nonblank text verbatim, including multiline
/// content; blankness follows `trim().is_empty()`. The heading and row shape come
/// from the run's prompt profile (`role.listing_heading` / `role.listing_row`).
pub(crate) fn append_role_descriptions(
    schema: &mut Value,
    roles: &[AgentRoleSummary],
    profile: &PromptProfile,
) {
    let lines = roles
        .iter()
        .filter(|role| !role.description.trim().is_empty())
        .map(|role| {
            profile.render(
                PromptKey::RoleListingRow,
                &[("name", &role.name), ("description", &role.description)],
            )
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return;
    }
    let Some(description) = schema["description"].as_str() else {
        return;
    };
    let block = format!(
        "{description}\n\n{}\n{}",
        profile.text(PromptKey::RoleListingHeading),
        lines.join("\n")
    );
    schema["description"] = json!(block);
}

/// Schema of the child-only `subagent_update` tool, with its prose from the
/// run's prompt profile.
pub(crate) fn subagent_update_schema(profile: &PromptProfile) -> Value {
    json!({
        "type": "object",
        "description": profile.text(PromptKey::SubagentUpdateToolDescription),
        "properties": {
            "message": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4000,
                "description": profile.text(PromptKey::SubagentUpdateMessageDescription)
            }
        },
        "required": ["message"],
        "additionalProperties": false
    })
}

/// Model-visible `send_message` schema for a CHILD run.
///
/// The public catalog schema takes `{target, message}` because the main agent
/// picks which of its children to write to. A child has exactly one reachable
/// correspondent — the agent that spawned it — so `target` is dropped rather
/// than constrained: a parameter whose only valid value the host already knows
/// is an invitation to address a sibling and be refused.
pub(crate) fn child_send_message_schema(profile: &PromptProfile) -> Value {
    json!({
        "type": "object",
        "description": profile.text(PromptKey::SubagentSendMainToolDescription),
        "properties": {
            "message": {
                "type": "string",
                "minLength": 1,
                "maxLength": 32768,
                "description": profile.text(PromptKey::SubagentSendMainMessageDescription)
            }
        },
        "required": ["message"],
        "additionalProperties": false
    })
}

/// Model-visible `skill` schema.
///
/// It says nothing about which skills exist, and that is deliberate. A schema
/// is part of the tool set, and the tool set is re-declared on every request:
/// an `enum` of skill names would change the moment the user selected another
/// skill, mid-transcript, on protocols that will not take a changed tool set at
/// all. So the names — and what each one is for — live in the conversation's
/// own context, where a later addition is an appended message rather than a
/// rewrite. Prose comes from the profile (`skill.tool_description`,
/// `skill.name_description`); an unknown name is answered by
/// `api::run_skill_tool`, which names the ones this conversation has.
fn skill_schema(profile: &PromptProfile) -> Value {
    json!({
        "type": "object",
        "description": profile.text(PromptKey::SkillToolDescription),
        "properties": {
            "name": {
                "type": "string",
                "minLength": 1,
                "maxLength": 240,
                "description": profile.text(PromptKey::SkillNameDescription)
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

/// Model-visible `tool_search` schema.
///
/// It names no tool, for the same reason [`skill_schema`] names no skill: a
/// schema is part of the tool set, the tool set is re-declared on every
/// request, and an `enum` of deferred tool names would move the moment a server
/// answered differently. The names live in the run's own announcement context,
/// where a later MCP server is an appended message rather than a rewrite, and a
/// name this run does not hold is answered by `api::run_tool_search_tool`.
fn tool_search_schema(profile: &PromptProfile) -> Value {
    json!({
        "type": "object",
        "description": profile.text(PromptKey::ToolSearchToolDescription),
        "properties": {
            "query": {
                "type": "string",
                "minLength": 1,
                "maxLength": 2048,
                "description": profile.text(PromptKey::ToolSearchQueryDescription)
            },
            "max_results": {
                "type": "integer",
                "minimum": 1,
                "maximum": 50,
                "default": 5,
                "description": profile.text(PromptKey::ToolSearchMaxResultsDescription)
            }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tool_catalog;
    use crate::model::{AttachedWorkspace, ExecutionEnvironmentAssets, RunTarget, SshMachineConfig};
    use std::collections::BTreeSet;

    /// A set with the host workspace first and an SSH one second, which is the
    /// shape every rule about the parameter turns on.
    fn mixed_workspaces() -> WorkspaceSet {
        let assets = ExecutionEnvironmentAssets {
            ssh_machines: vec![SshMachineConfig {
                id: "m1".into(),
                name: "devbox".into(),
                host: "user@devbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        WorkspaceSet::resolve(
            &assets,
            &AttachedWorkspace {
                machine: None,
                path: "C:/work/app".into(),
            },
            &[AttachedWorkspace {
                machine: Some(RunTarget::Ssh {
                    machine_id: "m1".into(),
                }),
                path: "~/services".into(),
            }],
        )
        .unwrap()
    }

    fn schema_with_workspaces(name: &str, workspaces: &WorkspaceSet) -> Value {
        let schema = builtin_tool_schema(name, &PromptProfile::builtin_english())
            .unwrap_or_else(|| panic!("no schema for {name}"));
        with_workspace_parameter(schema, name, workspaces)
    }

    /// One workspace is not a choice, so the parameter is not a question worth
    /// asking — and an enum of one value is a token cost with no answer in it.
    #[test]
    fn a_single_workspace_adds_no_parameter() {
        let single = WorkspaceSet::local_root("C:/work/app");
        for name in ["read", "bash", "powershell", "grep"] {
            assert!(
                !properties(&schema_with_workspaces(name, &single)).contains("workspace"),
                "{name} should carry no workspace parameter"
            );
        }
    }

    #[test]
    fn every_path_and_shell_tool_gains_the_parameter_once_there_is_a_choice() {
        let workspaces = mixed_workspaces();
        for name in ["ls", "grep", "find", "read", "write", "edit", "lsp", "bash"] {
            let schema = schema_with_workspaces(name, &workspaces);
            assert!(
                properties(&schema).contains("workspace"),
                "{name} should address a workspace"
            );
            assert_eq!(schema["properties"]["workspace"]["enum"], json!([1, 2]));
            assert_eq!(schema["properties"]["workspace"]["default"], json!(1));
        }
    }

    /// The page tools, memory and orchestration tools are host subsystems rather
    /// than things that happen in a directory, so a number would advertise a
    /// choice that changes nothing.
    #[test]
    fn tools_that_do_not_act_in_a_directory_are_left_alone() {
        let workspaces = mixed_workspaces();
        for name in ["preview_stop", "preview_screenshot", "ask_user", "todo"] {
            let schema = schema_with_workspaces(name, &workspaces);
            // `todo` is a `oneOf` with no root properties at all, which is also
            // the shape the injector has to leave untouched rather than crash on.
            assert!(
                schema["properties"]["workspace"].is_null(),
                "{name} should not address a workspace: {schema}"
            );
        }
    }

    /// `preview_start` does name one: whose `.mework/launch.json` it reads, and
    /// so on which machine the server runs — an SSH machine's included.
    #[test]
    fn preview_start_names_the_workspace_whose_server_it_runs() {
        let workspaces = mixed_workspaces();
        let schema = schema_with_workspaces("preview_start", &workspaces);
        assert_eq!(schema["properties"]["workspace"]["enum"], json!([1, 2]));
        assert_eq!(schema["required"], json!(["name"]));
    }

    /// A shell tool lists only the workspaces whose machine has its shell: the
    /// unprobed SSH machine here is assumed to have bash alone, so `powershell`
    /// can name at most the host's workspace, and `bash` names both.
    #[test]
    fn a_shell_tool_lists_only_the_workspaces_whose_machine_has_its_shell() {
        let workspaces = mixed_workspaces();
        let schema = schema_with_workspaces("powershell", &workspaces);
        let host_has_powershell = crate::machine_shells::local()
            .get(crate::shell_backend::ShellBackend::PowerShell)
            .is_some();
        if host_has_powershell {
            assert_eq!(schema["properties"]["workspace"]["enum"], json!([1]));
            let description = schema["properties"]["workspace"]["description"]
                .as_str()
                .unwrap_or_default();
            assert!(
                description.contains("PowerShell") && description.contains("another shell tool"),
                "a narrowed list has to say why and where to go instead: {description}"
            );
        } else {
            // No machine here has PowerShell, so the tool is withdrawn from the
            // request and never reaches this function.
            assert!(!properties(&schema).contains("workspace"));
        }
        let bash = schema_with_workspaces("bash", &workspaces);
        if crate::machine_shells::local()
            .get(crate::shell_backend::ShellBackend::Bash)
            .is_some()
        {
            assert_eq!(bash["properties"]["workspace"]["enum"], json!([1, 2]));
        }
    }

    fn properties(schema: &Value) -> BTreeSet<String> {
        schema["properties"]
            .as_object()
            .expect("schema properties object")
            .keys()
            .cloned()
            .collect()
    }

    /// `todo` is a discriminated union, so its shape claims hold per variant. Every
    /// other tool has exactly one shape and is checked directly.
    fn schema_shapes(schema: &Value) -> Vec<Value> {
        match schema["oneOf"].as_array() {
            Some(variants) => variants.clone(),
            None => vec![schema.clone()],
        }
    }

    /// Role names must be machine-readable in both role-naming tool schemas:
    /// `agent_spawn` uses `agent_type.enum`; `workflow` uses `$defs.agentType` because
    /// `agentType` occurs in JavaScript source.
    #[test]
    fn configured_role_names_become_enum_values_in_both_role_naming_tools() {
        let names = vec!["alpha".to_owned(), "zeta".to_owned()];

        let spawn = agent_spawn_schema_for_roles(&names, false, &PromptProfile::builtin_english());
        assert_eq!(
            spawn["properties"]["agent_type"]["enum"],
            json!(["alpha", "zeta"])
        );
        // The `not` guard references `agent_type` and must remain with roles.
        assert!(spawn.get("not").is_some());

        let workflow = workflow_schema_for_roles(&names, false, &PromptProfile::builtin_english());
        assert_eq!(
            workflow["$defs"]["agentType"]["enum"],
            json!(["alpha", "zeta"])
        );
        // The script prose points to `$defs` rather than repeating role names.
        let description = workflow["properties"]["script"]["description"]
            .as_str()
            .expect("script description");
        assert!(description.contains("$defs.agentType"), "{description}");
        assert!(
            !description.contains("alpha"),
            "名字只该出现在 enum 里：{description}"
        );
    }

    /// Without roles, remove `agent_type` and its guard because the field has no legal
    /// value.
    #[test]
    fn a_conversation_without_roles_loses_the_agent_type_field_entirely() {
        let spawn = agent_spawn_schema_for_roles(&[], false, &PromptProfile::builtin_english());
        assert!(spawn["properties"].get("agent_type").is_none(), "{spawn}");
        assert!(spawn.get("not").is_none(), "{spawn}");
        // Only the role selector is removed; other tool properties remain.
        assert!(spawn["properties"].get("prompt").is_some());
        assert!(spawn["properties"].get("schema").is_some());

        let workflow = workflow_schema_for_roles(&[], false, &PromptProfile::builtin_english());
        assert!(workflow.get("$defs").is_none(), "{workflow}");
        let description = workflow["properties"]["script"]["description"]
            .as_str()
            .expect("script description");
        assert!(
            description.contains("no named agent configured"),
            "没有角色时要说清楚，而不是继续描述一个用不了的选项：{description}"
        );
    }

    /// The `skill` schema is fixed: it names no skill and lists no trigger, so
    /// selecting another skill mid-conversation leaves the tool set untouched.
    #[test]
    fn the_skill_schema_names_no_skill_and_lists_no_trigger() {
        let schema = builtin_tool_schema("skill", &PromptProfile::builtin_english())
            .expect("skill has a static schema");

        assert!(
            schema["properties"]["name"].get("enum").is_none(),
            "{schema}"
        );
        let description = schema["description"].as_str().expect("skill description");
        assert!(
            !description.contains(PromptKey::SkillListingHeading.builtin_en()),
            "{description}"
        );
        // Nor may the parameter's own prose promise a listing the schema does
        // not carry: `capabilities::runtime_context` puts it in the context.
        let name_description = schema["properties"]["name"]["description"]
            .as_str()
            .expect("name description");
        assert!(!name_description.contains("enum"), "{name_description}");
        // The static baseline must retain its complete property surface for golden and
        // catalog consistency comparisons.
        assert_eq!(schema["required"], json!(["name"]));
    }

    /// Required-role mode requires `agent_type` and removes `context` with its `not`
    /// guard because conversation context and named roles are mutually exclusive.
    ///
    #[test]
    fn requiring_a_role_puts_agent_type_in_required_and_removes_context() {
        let names = vec!["alpha".to_owned()];

        let required =
            agent_spawn_schema_for_roles(&names, true, &PromptProfile::builtin_english());
        assert_eq!(
            required["required"],
            json!(["prompt", "name", "agent_type"])
        );
        assert!(
            required["properties"].get("context").is_none(),
            "{required}"
        );
        assert!(required.get("not").is_none(), "{required}");
        // Required mode retains the role enum; it narrows requiredness, not choices.
        assert_eq!(
            required["properties"]["agent_type"]["enum"],
            json!(["alpha"])
        );
        // Other properties remain.
        for key in ["prompt", "name", "label", "schema"] {
            assert!(
                required["properties"].get(key).is_some(),
                "{key}: {required}"
            );
        }

        let fallback =
            agent_spawn_schema_for_roles(&names, false, &PromptProfile::builtin_english());
        // Fallback relaxes only role selection; `prompt` and generated task address
        // `name` are required in both modes.
        assert_eq!(fallback["required"], json!(["prompt", "name"]));
        assert!(fallback["properties"].get("context").is_some());
        assert!(fallback.get("not").is_some());
    }

    /// Because `agentType` exists in JavaScript source beyond schema enforcement,
    /// required-role prose must state both the rule and synchronous failure behavior.
    #[test]
    fn requiring_a_role_states_both_the_rule_and_its_enforcement_in_the_script_prose() {
        let names = vec!["alpha".to_owned()];
        let script_prose = |required: bool| {
            workflow_schema_for_roles(&names, required, &PromptProfile::builtin_english())
                ["properties"]["script"]["description"]
                .as_str()
                .expect("script description")
                .to_owned()
        };

        let required = script_prose(true);
        assert!(required.contains("REQUIRED"), "{required}");
        assert!(required.contains("$defs.agentType"), "{required}");
        assert!(
            required.contains("throws synchronously"),
            "要说清楚强制发生在哪里：{required}"
        );
        assert!(
            required.contains("NOT a step that resolves to null"),
            "要否掉「大不了这一步是 null」这个读法：{required}"
        );
        // Required mode makes `opts` non-optional in the signature.
        assert!(required.contains("agent(prompt, opts) ->"), "{required}");

        let fallback = script_prose(false);
        assert!(!fallback.contains("REQUIRED"), "{fallback}");
        assert!(fallback.contains("agent(prompt, opts?) ->"), "{fallback}");
    }

    /// Role descriptions append a titled `- <name>: <description>` list without
    /// modifying the original description.
    #[test]
    fn role_descriptions_append_a_titled_list_after_the_original_description() {
        let mut schema = json!({"description": "Tool prose.", "type": "object"});
        append_role_descriptions(
            &mut schema,
            &[
                role("reviewer", "对抗式审查：负责证伪既有结论。"),
                role("researcher", "深入检索与资料汇总。"),
            ],
            &PromptProfile::builtin_english(),
        );
        assert_eq!(
            schema["description"],
            json!(
                "Tool prose.\n\nAvailable agent types:\n\
                 - reviewer: 对抗式审查：负责证伪既有结论。\n\
                 - researcher: 深入检索与资料汇总。"
            )
        );
        // No schema key outside the description changes.
        assert_eq!(schema["type"], json!("object"));
    }

    /// Omit roles with blank descriptions; whitespace-only descriptions are blank.
    #[test]
    fn a_role_without_a_description_contributes_no_line() {
        let mut schema = json!({"description": "Tool prose."});
        append_role_descriptions(
            &mut schema,
            &[
                role("quiet", ""),
                role("spacey", "   \n  "),
                role("loud", "说点什么。"),
            ],
            &PromptProfile::builtin_english(),
        );
        assert_eq!(
            schema["description"],
            json!("Tool prose.\n\nAvailable agent types:\n- loud: 说点什么。")
        );
    }

    /// Omit the role block when no description produces an entry.
    #[test]
    fn an_all_silent_role_set_leaves_the_description_untouched() {
        for roles in [vec![], vec![role("quiet", ""), role("also-quiet", "  ")]] {
            let mut schema = json!({"description": "Tool prose."});
            append_role_descriptions(&mut schema, &roles, &PromptProfile::builtin_english());
            assert_eq!(schema["description"], json!("Tool prose."), "{roles:?}");
        }
    }

    /// Preserve multiline descriptions verbatim without trimming, wrapping, escaping,
    /// or rendering.
    #[test]
    fn a_multi_line_description_is_written_through_verbatim() {
        let mut schema = json!({"description": "Tool prose."});
        append_role_descriptions(
            &mut schema,
            &[role("multi", "第一行。\n第二行 - 带个横线。\n\n第四行。")],
            &PromptProfile::builtin_english(),
        );
        assert_eq!(
            schema["description"],
            json!(
                "Tool prose.\n\nAvailable agent types:\n\
                 - multi: 第一行。\n第二行 - 带个横线。\n\n第四行。"
            )
        );
    }

    /// Both role-naming tools share the same renderer, so their appended blocks must
    /// be byte-identical.
    #[test]
    fn both_role_naming_tools_receive_a_byte_identical_block() {
        let names = vec!["reviewer".to_owned()];
        let roles = vec![role("reviewer", "证伪既有结论。")];

        let mut spawn =
            agent_spawn_schema_for_roles(&names, false, &PromptProfile::builtin_english());
        let spawn_base = spawn["description"]
            .as_str()
            .expect("spawn description")
            .to_owned();
        append_role_descriptions(&mut spawn, &roles, &PromptProfile::builtin_english());
        let spawn_block = spawn["description"]
            .as_str()
            .expect("spawn description")
            .strip_prefix(&spawn_base)
            .expect("块必须追加在原描述之后")
            .to_owned();

        let mut workflow =
            workflow_schema_for_roles(&names, false, &PromptProfile::builtin_english());
        let workflow_base = workflow["description"]
            .as_str()
            .expect("workflow description")
            .to_owned();
        append_role_descriptions(&mut workflow, &roles, &PromptProfile::builtin_english());
        let workflow_block = workflow["description"]
            .as_str()
            .expect("workflow description")
            .strip_prefix(&workflow_base)
            .expect("块必须追加在原描述之后")
            .to_owned();

        assert_eq!(spawn_block, workflow_block);
        assert_eq!(
            spawn_block,
            "\n\nAvailable agent types:\n- reviewer: 证伪既有结论。"
        );
        // The machine-readable enum remains alongside the description block.
        assert_eq!(
            spawn["properties"]["agent_type"]["enum"],
            json!(["reviewer"])
        );
        assert_eq!(workflow["$defs"]["agentType"]["enum"], json!(["reviewer"]));
    }

    fn role(name: &str, description: &str) -> AgentRoleSummary {
        AgentRoleSummary {
            name: name.to_owned(),
            description: description.to_owned(),
        }
    }

    /// The script description must fully state isolation behavior: its sole value,
    /// committed-tree boundary, retention rule, and cost.
    #[test]
    fn the_script_description_states_what_isolation_actually_does() {
        let description = workflow_schema(None, false, &PromptProfile::builtin_english())
            ["properties"]["script"]["description"]
            .as_str()
            .expect("script description")
            .to_owned();
        for needle in [
            "isolation",
            "worktree",
            "HEAD",
            "uncommitted",
            "EXPENSIVE",
            "git repository root",
        ] {
            assert!(description.contains(needle), "缺少 {needle}：{description}");
        }
    }

    #[test]
    fn every_public_tool_has_a_builtin_schema() {
        for tool in tool_catalog() {
            assert!(
                builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).is_some(),
                "{} lacks a hand-authored schema",
                tool.name
            );
        }
    }

    #[test]
    fn every_builtin_schema_is_a_closed_object_with_a_factual_description() {
        for tool in tool_catalog() {
            let schema =
                builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).unwrap();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            let description = schema["description"].as_str().unwrap_or_default();
            assert!(
                !description.trim().is_empty(),
                "{} schema must state what the operation is",
                tool.name
            );
            // Closedness is a per-shape claim: a union root carries no properties of its own, so
            // asserting it there would pass vacuously while a variant stayed open.
            for shape in schema_shapes(&schema) {
                assert_eq!(shape["type"], "object", "{}", tool.name);
                assert_eq!(shape["additionalProperties"], false, "{}", tool.name);
                let shape_description = shape["description"].as_str().unwrap_or_default();
                assert!(
                    !shape_description.trim().is_empty(),
                    "{} schema variant must state what the operation is",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn schema_properties_match_catalog_parameters() {
        for tool in tool_catalog() {
            let schema =
                builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).unwrap();
            let schema_properties: BTreeSet<String> = schema_shapes(&schema)
                .iter()
                .flat_map(|shape| properties(shape))
                .collect();
            let parameters: BTreeSet<String> = tool
                .parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect();
            match tool.name.as_str() {
                // Permission-component fields are not Composer parameters and therefore
                // are intentionally absent from the wire schema.
                "ask_user" => {
                    assert!(schema_properties.is_superset(&parameters), "ask_user");
                }
                // The catalog lists what the widest form takes: the base schema plus the
                // decision pair, which is exactly what `Augment` declares.
                name if crate::decision_tools::takes_decision_parameters(name) => {
                    let augmented = with_decision_parameters(
                        schema.clone(),
                        name,
                        crate::model::DecisionParameterMode::Augment,
                    );
                    assert_eq!(
                        properties(&augmented),
                        parameters,
                        "{name}: augmented schema properties and catalog parameters drifted"
                    );
                }
                _ => {
                    assert_eq!(
                        schema_properties, parameters,
                        "{}: schema properties and catalog parameters drifted",
                        tool.name
                    );
                }
            }
        }
    }

    /// `Augment` keeps every direct parameter and makes the target optional; `Replace` withdraws
    /// the target and requires the decision parameters. Whatever else the tool takes survives both.
    #[test]
    fn decision_parameter_modes_reshape_only_the_target() {
        use crate::model::DecisionParameterMode::{Augment, Replace};
        let profile = PromptProfile::builtin_english();
        let fill = builtin_tool_schema("preview_fill", &profile).unwrap();

        // The element tools choose one element or none of the above: `query`, and no threshold.
        let augmented = with_decision_parameters(fill.clone(), "preview_fill", Augment);
        assert_eq!(
            properties(&augmented),
            ["query", "selector", "serverId", "value"]
                .map(String::from)
                .into_iter()
                .collect()
        );
        assert_eq!(augmented["required"], json!(["value"]));
        assert!(augmented["description"]
            .as_str()
            .unwrap()
            .contains("none of the above"));

        let replaced = with_decision_parameters(fill, "preview_fill", Replace);
        assert_eq!(
            properties(&replaced),
            ["query", "serverId", "value"]
                .map(String::from)
                .into_iter()
                .collect()
        );
        assert_eq!(replaced["required"], json!(["value", "query"]));
        assert_eq!(replaced["additionalProperties"], false);

        // The console and snapshot tools have no target to withdraw: `Replace` only makes the
        // decision model mandatory.
        let snapshot = builtin_tool_schema("preview_snapshot", &profile).unwrap();
        let replaced = with_decision_parameters(snapshot, "preview_snapshot", Replace);
        assert_eq!(replaced["required"], json!(["query", "threshold"]));
        assert!(properties(&replaced).contains("threshold"));
        let console = builtin_tool_schema("preview_console_logs", &profile).unwrap();
        let augmented = with_decision_parameters(console, "preview_console_logs", Augment);
        assert_eq!(augmented["required"], json!([]));
        assert!(properties(&augmented).contains("level"));
    }

    /// The preview surface is sixteen independent tools, not one multiplexed one. This
    /// pins the whole set — including the two Mework-only tools the source has no
    /// equivalent for — so a tool cannot be dropped or renamed without a decision.
    #[test]
    fn the_catalog_carries_every_preview_tool_and_no_multiplexed_browser_tool() {
        let names: Vec<String> = tool_catalog()
            .iter()
            .map(|tool| tool.name.clone())
            .filter(|name| name.starts_with("preview_"))
            .collect();
        assert_eq!(
            names,
            [
                "preview_start",
                "preview_stop",
                "preview_list",
                "preview_logs",
                "preview_console_logs",
                "preview_screenshot",
                "preview_snapshot",
                "preview_inspect",
                "preview_click",
                "preview_fill",
                "preview_eval",
                "preview_network",
                "preview_resize",
                "preview_upload_image",
                "preview_dialog",
                "preview_find_logs",
            ]
        );
        assert!(!tool_catalog().iter().any(|tool| tool.name == "playwright"));
        for name in &names {
            let schema = builtin_tool_schema(name, &PromptProfile::builtin_english()).unwrap();
            assert!(
                schema.get("oneOf").is_none(),
                "{name} must be a flat tool, not a discriminated union"
            );
        }
    }

    /// `serverId` is optional everywhere it appears, against the source schema, because the
    /// dispatcher resolves it when absent. `preview_stop` is the exception: there is nothing
    /// to fall back to when the caller does not say which server to kill.
    #[test]
    fn server_id_is_optional_on_every_preview_tool_except_stop() {
        for tool in tool_catalog() {
            if !tool.name.starts_with("preview_") {
                continue;
            }
            let schema =
                builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).unwrap();
            if schema["properties"].get("serverId").is_none() {
                assert!(
                    tool.name == "preview_start" || tool.name == "preview_list",
                    "{} needs a serverId",
                    tool.name
                );
                continue;
            }
            let required: Vec<&str> = schema["required"]
                .as_array()
                .expect("required")
                .iter()
                .map(|entry| entry.as_str().expect("required entry"))
                .collect();
            assert_eq!(
                required.contains(&"serverId"),
                tool.name == "preview_stop",
                "{} serverId requiredness",
                tool.name
            );
        }
    }

    /// The launch.json template and its prose are one text, owned by
    /// `preview_launch_config`. `preview_start`'s description quotes them, and both
    /// built-in profiles must quote the same bytes — a paraphrase there would teach the
    /// model a file format the parser rejects.
    #[test]
    fn preview_start_descriptions_quote_the_launch_json_format_verbatim() {
        let english = format!(
            "Start a dev server by name from .mework/launch.json. If .mework/launch.json doesn't exist, create it first with this format:\n{}\n{} Reuses the server if already running. ALWAYS use this instead of Bash for running servers. If the deliverable is already published as an Artifact, update the Artifact instead of starting a server to show it.",
            crate::preview_launch_config::LAUNCH_JSON_FORMAT,
            crate::preview_launch_config::LAUNCH_JSON_FORMAT_NOTES
        );
        assert_eq!(
            PromptProfile::builtin_english().text(PromptKey::ToolPreviewStartDescription),
            english
        );
        assert!(PromptProfile::builtin_chinese()
            .text(PromptKey::ToolPreviewStartDescription)
            .contains(crate::preview_launch_config::LAUNCH_JSON_FORMAT));
    }

    #[test]
    fn required_entries_reference_declared_properties() {
        for tool in tool_catalog() {
            let schema =
                builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).unwrap();
            for shape in schema_shapes(&schema) {
                let shape_properties = properties(&shape);
                if let Some(required) = shape["required"].as_array() {
                    for entry in required {
                        let name = entry.as_str().expect("required entry string");
                        assert!(
                            shape_properties.contains(name),
                            "{}: required {name} is not a declared property",
                            tool.name
                        );
                    }
                }
            }
        }
    }

    /// Sole authoritative generator for `docs/context-injections/builtin-tool-schemas.json`.
    ///
    /// Golden-file comparison keeps the design baseline current without parsing `json!`.
    fn baseline_document() -> String {
        let tools: Vec<Value> = tool_catalog()
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "schema": builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).expect("public tool schema"),
                })
            })
            .collect();
        let document = json!({
            "kind": "mework-builtin-tool-schema-baseline",
            "note": "Model-visible parameter schemas (context layer 2: what things are, with every boundary as a JSON Schema keyword). Generated by builtin_schemas.rs tests; regenerate with: cargo test --lib -- builtin_schemas::tests::regenerate_builtin_schema_baseline --ignored",
            "source": "src-tauri/src/builtin_schemas.rs::builtin_tool_schema",
            "toolCount": tools.len(),
            "tools": tools,
            "internalTools": {
                "subagent_update": subagent_update_schema(&PromptProfile::builtin_english()),
            },
        });
        let mut rendered = serde_json::to_string_pretty(&document).expect("baseline JSON");
        rendered.push('\n');
        rendered
    }

    fn baseline_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/context-injections/builtin-tool-schemas.json")
    }

    #[test]
    fn builtin_schema_baseline_is_current() {
        let expected = baseline_document();
        let current = std::fs::read_to_string(baseline_path()).unwrap_or_default();
        assert!(
            current == expected,
            "docs/context-injections/builtin-tool-schemas.json 已过期；运行\n  cargo test --lib -- builtin_schemas::tests::regenerate_builtin_schema_baseline --ignored\n重新生成后一并提交"
        );
    }

    #[test]
    #[ignore = "writes the design baseline under docs/; run explicitly to regenerate"]
    fn regenerate_builtin_schema_baseline() {
        std::fs::write(baseline_path(), baseline_document()).expect("write baseline");
    }

    /// Same union contract as `todo`: the merged state tool must expose exactly the
    /// actions its executor dispatches on, each discriminated by a required
    /// `action` const. A variant the executor cannot run — or an action with no variant — would
    /// be a schema the model can call into a dead end.
    #[test]
    fn state_tool_variants_cover_every_action_exactly_once() {
        for (tool, expected) in [("todo", crate::orchestration::TODO_ACTIONS)] {
            let schema = builtin_tool_schema(tool, &PromptProfile::builtin_english()).unwrap();
            let variants = schema["oneOf"].as_array().expect("state tool oneOf");
            let declared: Vec<&str> = variants
                .iter()
                .map(|variant| {
                    let action = variant["properties"]["action"]["const"]
                        .as_str()
                        .expect("action const");
                    let required: Vec<&str> = variant["required"]
                        .as_array()
                        .expect("variant required")
                        .iter()
                        .map(|entry| entry.as_str().expect("required entry"))
                        .collect();
                    assert!(
                        required.contains(&"action"),
                        "{tool} {action} must require action"
                    );
                    action
                })
                .collect();
            assert_eq!(declared, expected, "{tool}");
            assert_eq!(
                declared.iter().collect::<BTreeSet<_>>().len(),
                declared.len(),
                "{tool}"
            );
        }
    }

    #[test]
    fn task_wait_bounds_track_agents_constants() {
        let schema = builtin_tool_schema("task_wait", &PromptProfile::builtin_english()).unwrap();
        let timeout = &schema["properties"]["timeout_seconds"];
        assert_eq!(timeout["minimum"], json!(WAIT_MIN_TIMEOUT_SECONDS));
        assert_eq!(timeout["maximum"], json!(WAIT_MAX_TIMEOUT_SECONDS));
        assert_eq!(timeout["default"], json!(WAIT_DEFAULT_TIMEOUT_SECONDS));
        assert_eq!(
            schema["properties"]["tasks"]["maxItems"],
            json!(MAX_WAIT_AGENT_NAMES)
        );
    }
}
