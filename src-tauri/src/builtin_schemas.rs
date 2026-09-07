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

/// Host format for a snapshot ref: `e` followed by a nonzero 1- to 10-digit decimal number.
const SNAPSHOT_REF_PATTERN: &str = "^e[1-9][0-9]{0,9}$";

fn string_prop(description: &str, max_length: usize) -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": max_length,
        "description": description
    })
}

fn selector_prop() -> Value {
    string_prop("CSS selector of the target element.", 2048)
}

fn ref_prop() -> Value {
    json!({
        "type": "string",
        "pattern": SNAPSHOT_REF_PATTERN,
        "description": "Element ref from the latest playwright snapshot."
    })
}

/// Require exactly one of selector and ref; the host rejects both or neither.
fn selector_xor_ref() -> Value {
    json!([
        {"required": ["selector"]},
        {"required": ["ref"]}
    ])
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

/// Every `playwright` operation, in catalog order.
///
/// Order matters only for readability; `oneOf` matching is by the `action` const.
fn playwright_variants() -> Vec<Value> {
    vec![
        action_variant(
            "navigate",
            "Navigate the current tab and wait for the new document: the call fails after 60 s without DOMContentLoaded, then waits up to 5 s more for load. The page is created in the background on first use. The result carries the page header and an accessibility snapshot of the new document.",
            json!({
                "properties": {
                    "url": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 8192,
                        "description": "An http(s) URL or about:blank, or one of the history commands back, forward, reload."
                    }
                },
                "required": ["url"]
            }),
        ),
        action_variant(
            "snapshot",
            "Read the page as a YAML accessibility tree: roles, accessible names, states, input values and interactable element refs, including same-origin iframes. The header carries URL, title, viewport and recent dialog records. Interactions already return a bounded copy of this tree; call snapshot for the full tree or fresh refs.",
            json!({
                "properties": {
                    "max_chars": {
                        "type": "integer",
                        "minimum": 1000,
                        "maximum": 60000,
                        "default": 30000,
                        "description": "Snapshot size cap in characters."
                    }
                }
            }),
        ),
        action_variant(
            "click",
            "Click an element after waiting up to 5 s for it to be visible, stable and unobstructed. The click is dispatched through the real browser input pipeline, and the call returns only after what it triggered has settled: a navigation it started is loaded, requests it started have finished. The result carries the page header, a bounded snapshot, and any dialog the click opened.",
            json!({
                "properties": {
                    "selector": selector_prop(),
                    "ref": ref_prop(),
                    "button": {
                        "type": "string",
                        "enum": ["left", "right", "middle"],
                        "default": "left",
                        "description": "Mouse button."
                    },
                    "double": {
                        "type": "boolean",
                        "default": false,
                        "description": "Double-click instead of single-click."
                    },
                    "modifiers": {
                        "type": "array",
                        "items": {"type": "string", "enum": ["Control", "Shift", "Alt", "Meta"]},
                        "uniqueItems": true,
                        "description": "Modifier keys held during the click."
                    }
                },
                "oneOf": selector_xor_ref()
            }),
        ),
        action_variant(
            "type",
            "Type text into an editable element. By default the value is inserted in bulk; slowly dispatches real per-key events and caps the text at 2,000 characters.",
            json!({
                "properties": {
                    "selector": selector_prop(),
                    "ref": ref_prop(),
                    "text": {
                        "type": "string",
                        "maxLength": 1000000,
                        "description": "Text to type."
                    },
                    "clear": {
                        "type": "boolean",
                        "default": true,
                        "description": "Clear the current value first."
                    },
                    "submit": {
                        "type": "boolean",
                        "default": false,
                        "description": "Press Enter after typing."
                    },
                    "slowly": {
                        "type": "boolean",
                        "default": false,
                        "description": "Dispatch individual key events."
                    }
                },
                "required": ["text"],
                "oneOf": selector_xor_ref()
            }),
        ),
        action_variant(
            "fill_form",
            "Fill several form fields in one call, in order, stopping at the first failure. Returns a per-field result list.",
            json!({
                "properties": {
                    "fields": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 50,
                        "description": "Fields to fill.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "selector": selector_prop(),
                                "ref": ref_prop(),
                                "value": {
                                    "type": "string",
                                    "maxLength": 16384,
                                    "description": "Text value for text-like inputs."
                                },
                                "values": {
                                    "description": "Option value(s) for select elements.",
                                    "oneOf": [
                                        {"type": "string", "maxLength": 16384},
                                        {
                                            "type": "array",
                                            "minItems": 1,
                                            "maxItems": 100,
                                            "items": {"type": "string", "maxLength": 16384}
                                        }
                                    ]
                                },
                                "checked": {
                                    "type": "boolean",
                                    "description": "Checked state for checkboxes and radio buttons."
                                }
                            },
                            "allOf": [
                                {"oneOf": [{"required": ["selector"]}, {"required": ["ref"]}]},
                                {"oneOf": [
                                    {"required": ["value"]},
                                    {"required": ["values"]},
                                    {"required": ["checked"]}
                                ]}
                            ],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["fields"]
            }),
        ),
        action_variant(
            "select",
            "Select one or more options of a select element by option value, then dispatch input and change events.",
            json!({
                "properties": {
                    "selector": selector_prop(),
                    "ref": ref_prop(),
                    "values": {
                        "description": "Option value or values to select.",
                        "oneOf": [
                            {"type": "string", "maxLength": 16384},
                            {
                                "type": "array",
                                "minItems": 1,
                                "maxItems": 100,
                                "items": {"type": "string", "maxLength": 16384}
                            }
                        ]
                    }
                },
                "required": ["values"],
                "oneOf": selector_xor_ref()
            }),
        ),
        action_variant(
            "hover",
            "Move the pointer over an element; on desktop this dispatches a real mouse move, so CSS :hover and hover menus trigger.",
            json!({
                "properties": {
                    "selector": selector_prop(),
                    "ref": ref_prop()
                },
                "oneOf": selector_xor_ref()
            }),
        ),
        action_variant(
            "key",
            "Send a key or key combination to the page through the browser input pipeline; keys like Tab, Enter and Escape keep their native default behavior.",
            json!({
                "properties": {
                    "key": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 64,
                        "description": "Key name, optionally with + separated modifiers (Control, Shift, Alt, Meta), e.g. Enter or Control+L."
                    },
                    "repeat": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "default": 1,
                        "description": "How many times to press the key."
                    }
                },
                "required": ["key"]
            }),
        ),
        action_variant(
            "scroll",
            "Scroll the page, or the scroll container of a target element. With a target and no x/y the target is scrolled into view; with x/y the container scrolls by that delta.",
            json!({
                "properties": {
                    "x": {
                        "type": "number",
                        "minimum": -10000000,
                        "maximum": 10000000,
                        "default": 0,
                        "description": "Horizontal scroll delta in pixels."
                    },
                    "y": {
                        "type": "number",
                        "minimum": -10000000,
                        "maximum": 10000000,
                        "default": 600,
                        "description": "Vertical scroll delta in pixels."
                    },
                    "selector": selector_prop(),
                    "ref": ref_prop()
                },
                "not": {"required": ["selector", "ref"]}
            }),
        ),
        action_variant(
            "evaluate",
            "Evaluate JavaScript in the page and return the serialized result. await is supported and the final expression is the return value.",
            json!({
                "properties": {
                    "script": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 1000000,
                        "description": "JavaScript source to evaluate."
                    },
                    "ref": {
                        "type": "string",
                        "pattern": SNAPSHOT_REF_PATTERN,
                        "description": "Element ref bound as the element variable inside the script."
                    }
                },
                "required": ["script"]
            }),
        ),
        action_variant(
            "wait",
            "Wait until every given condition holds: an element is visible, text appears, text disappears, or the document finishes loading. With no condition this is a pure delay of timeout_ms.",
            json!({
                "properties": {
                    "selector": selector_prop(),
                    "text": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 65536,
                        "description": "Text that must appear in the page."
                    },
                    "text_gone": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 65536,
                        "description": "Text that must disappear from the page."
                    },
                    "load": {
                        "type": "boolean",
                        "default": false,
                        "description": "Wait until document.readyState is complete."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 30000,
                        "default": 5000,
                        "description": "Wait deadline in milliseconds."
                    }
                }
            }),
        ),
        action_variant(
            "screenshot",
            "Capture the viewport, the full page, or one element as a PNG saved under the workspace. The receipt carries an imageId usable with the upload_image action.",
            json!({
                "properties": {
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "(?i)\\.png$",
                        "default": "browser-screenshot.png",
                        "description": "Workspace-relative PNG path to save to."
                    },
                    "full_page": {
                        "type": "boolean",
                        "default": false,
                        "description": "Capture the entire page instead of the viewport."
                    },
                    "selector": selector_prop(),
                    "ref": ref_prop()
                },
                "required": ["path"],
                "not": {"required": ["selector", "ref"]}
            }),
        ),
        action_variant(
            "console",
            "Read captured console entries, script errors and unhandled promise rejections. Entries may contain sensitive values.",
            json!({
                "properties": {
                    "only_errors": {
                        "type": "boolean",
                        "default": false,
                        "description": "Return error-level entries only."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "default": 100,
                        "description": "Maximum entries to return."
                    },
                    "clear": {
                        "type": "boolean",
                        "default": false,
                        "description": "Clear captured entries after reading."
                    }
                }
            }),
        ),
        action_variant(
            "network",
            "Read captured fetch/XHR/resource requests with full URL, status and a text response-body preview.",
            json!({
                "properties": {
                    "filter": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 2048,
                        "description": "URL substring filter."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "default": 50,
                        "description": "Maximum entries to return."
                    },
                    "clear": {
                        "type": "boolean",
                        "default": false,
                        "description": "Clear captured entries after reading."
                    }
                }
            }),
        ),
        action_variant(
            "dialog",
            "Answer the dialog the page is blocked on. alert/confirm/prompt/beforeunload dialogs are held open by the host: the page stays blocked and every other action is refused with the modal state until this action accepts or dismisses it. Without an open dialog the action fails and lists the recent dialog records.",
            json!({
                "properties": {
                    "accept": {
                        "type": "boolean",
                        "default": true,
                        "description": "true accepts the open dialog (OK), false dismisses it (Cancel)."
                    },
                    "prompt_text": {
                        "type": "string",
                        "maxLength": 16384,
                        "description": "Answer for an open prompt dialog; used only when accepting."
                    }
                }
            }),
        ),
        action_variant(
            "file_upload",
            "Set workspace files on a page file input (input[type=file]). When the page opened a file chooser (reported as a modal state after a click), the chooser is held and the files go to its input, so selector/ref may be omitted; otherwise name the input.",
            json!({
                "properties": {
                    "selector": selector_prop(),
                    "ref": ref_prop(),
                    "paths": {
                        "description": "Workspace-relative file path or paths.",
                        "oneOf": [
                            {"type": "string", "minLength": 1, "maxLength": 4096},
                            {
                                "type": "array",
                                "minItems": 1,
                                "maxItems": 10,
                                "items": {"type": "string", "minLength": 1, "maxLength": 4096}
                            }
                        ]
                    }
                },
                "required": ["paths"],
                "not": {"required": ["selector", "ref"]}
            }),
        ),
        action_variant(
            "upload_image",
            "Set one conversation image attachment on a page file input (input[type=file]). Images are addressed by their conversation number or receipt imageId. Like file_upload, selector/ref may be omitted while the page holds an open file chooser.",
            json!({
                "properties": {
                    "image_id": {
                        "description": "Conversation image number (3, #3 or [Image #3]) or a receipt's 64-hex imageId.",
                        "oneOf": [
                            {"type": "string", "pattern": "^(#?[0-9]+|\\[Image #[0-9]+\\]|[0-9a-f]{64})$"},
                            {"type": "integer", "minimum": 1}
                        ]
                    },
                    "selector": selector_prop(),
                    "ref": ref_prop(),
                    "filename": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 128,
                        "description": "File name the page sees; defaults to the attachment's own name."
                    }
                },
                "required": ["image_id"],
                "not": {"required": ["selector", "ref"]}
            }),
        ),
        action_variant(
            "resize",
            "Set the built-in browser viewport size.",
            json!({
                "properties": {
                    "width": {
                        "type": "integer",
                        "minimum": 320,
                        "maximum": 7680,
                        "description": "Viewport width in logical pixels."
                    },
                    "height": {
                        "type": "integer",
                        "minimum": 240,
                        "maximum": 4320,
                        "description": "Viewport height in logical pixels."
                    }
                },
                "required": ["width", "height"]
            }),
        ),
        action_variant(
            "tab_new",
            "Open a new tab in this conversation's browser and make it the tab later actions run against. Each tab runs in its own single-use profile: it starts signed out everywhere, never sees another tab's cookies, and its state is destroyed when the tab closes. The tab appears in the sidebar without coming to the front.",
            json!({
                "properties": {
                    "url": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 8192,
                        "description": "http(s) URL to open; omitted leaves the tab blank."
                    }
                }
            }),
        ),
        action_variant(
            "tab_list",
            "List every browser tab of this conversation: tab id, URL, title, and which tab later actions run against. The primary tab's id is always main.",
            json!({"properties": {}}),
        ),
        action_variant(
            "tab_select",
            "Point every later action at the given tab. This moves only the tools' target, not which tab the interface shows.",
            json!({
                "properties": {
                    "tab": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 256,
                        "description": "Tab id from the tab_list action; the primary tab is main."
                    }
                },
                "required": ["tab"]
            }),
        ),
        action_variant(
            "tab_close",
            "Close the given tab and release its page. Closing the current tab makes the next tab in the list current; closing the last tab leaves none, and the next action opens a fresh blank page.",
            json!({
                "properties": {
                    "tab": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 256,
                        "description": "Tab id from the tab_list action; main is the conversation's own page."
                    }
                },
                "required": ["tab"]
            }),
        ),
        action_variant(
            "close",
            "Close every tab of this conversation's browser and release their pages. The next action opens a fresh blank page again.",
            json!({"properties": {}}),
        ),
    ]
}

/// Every `todo` operation, in catalog order.
///
/// Same merge shape as `playwright`: `action` is a `const` inside each variant,
/// so the model that picks an action sees exactly that action's parameters and
/// every boundary keyword of the pre-merge per-operation schemas survives —
/// including `update`'s "at least one patch field" `anyOf`.
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
        // ------------------------------------------------------------ Web search
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
        // ------------------------------------------------------------- Browser
        //
        // The browser surface is one wire tool multiplexed by `action`; each closed
        // variant preserves the selected operation's parameter boundaries.
        "playwright" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolPlaywrightDescription),
            "oneOf": playwright_variants()
        }),
        // -------------------------------------------------------------- Subagents
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
                        "description": "A child agent name, a workflow run name or workflow:<runId>, shell:<id>, or terminal:<id>."
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
        // ---------------------------------------------------------- Long-term memory
        "read_global_memory" => memory_read_schema(
            profile.text(PromptKey::ToolReadGlobalMemoryDescription),
        ),
        "read_project_memory" => memory_read_schema(
            profile.text(PromptKey::ToolReadProjectMemoryDescription),
        ),
        "create_global_memory" => memory_create_schema(
            profile.text(PromptKey::ToolCreateGlobalMemoryDescription),
        ),
        "create_project_memory" => memory_create_schema(
            profile.text(PromptKey::ToolCreateProjectMemoryDescription),
        ),
        "edit_global_memory" => memory_edit_schema(
            profile.text(PromptKey::ToolEditGlobalMemoryDescription),
        ),
        "edit_project_memory" => memory_edit_schema(
            profile.text(PromptKey::ToolEditProjectMemoryDescription),
        ),
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
                    "description": "First user message of the forked conversation, and the only instruction you will ever give it — there is no channel for a correction afterwards. State the task and every piece of background it needs: unless inherit_context is true, the child sees nothing else."
                },
                "inherit_context": {
                    "type": "boolean",
                    "default": false,
                    "description": "Optional, default false: the child starts with only the prompt. true copies this conversation's timeline so far and its completed tasks into the child as well. Inherit when the job continues this conversation's thread; start clean when it does not, so the child is not steered by history it has no use for."
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
        // Both mode tools are a request, not a payload: the plan they act on is
        // the stored document, and the answer is the user's.
        "exit_plan_mode" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolExitPlanModeDescription),
            "properties": {},
            "additionalProperties": false
        }),
        "enter_plan_mode" => json!({
            "type": "object",
            "description": profile.text(PromptKey::ToolEnterPlanModeDescription),
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
        "skill" => skill_schema(None, profile),
        _ => return None,
    };
    Some(schema)
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
fn workflow_schema(roles: Option<&[String]>, role_required: bool, profile: &PromptProfile) -> Value {
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
                "description": "Required. Name this run yourself: it is this run's address in the same namespace agents are named in, and the title the task is listed under. Say what the run is for (review-sweep, migrate-callsites). The name is reserved for the whole conversation branch tree, so a resume of an earlier run still needs a fresh one."
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
                "description": "Run id reported by a previous run of this same script. Journaled steps replay instantly; the first unjournaled step and everything after it re-runs. On resume, script may be omitted — the host reloads the approved script from the run directory; if provided, it must be byte-identical to the approved one."
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
fn agent_spawn_schema(roles: Option<&[String]>, role_required: bool, profile: &PromptProfile) -> Value {
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

/// Model-visible `skill` schema.
///
/// `None` produces the static baseline and `Some` produces a run-specific schema.
/// Names are machine-readable constraints in `properties.name.enum`; triggers are prose
/// appended to the top-level description. The host does not derive this tool for an
/// empty skill set, so an empty enum must never be advertised. Prose comes from
/// the profile (`skill.tool_description`, `skill.name_description`,
/// `skill.listing_heading`, `skill.listing_row`).
fn skill_schema(skills: Option<&[(String, String)]>, profile: &PromptProfile) -> Value {
    let mut schema = json!({
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
    });
    let Some(entries) = skills.filter(|entries| !entries.is_empty()) else {
        return schema;
    };
    schema["properties"]["name"]["enum"] = json!(entries
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>());
    // Omit blank triggers; names already appear in the enum, and omit the whole block
    // if no nonblank trigger remains.
    let lines = entries
        .iter()
        .filter(|(_, trigger)| !trigger.trim().is_empty())
        .map(|(name, trigger)| {
            profile.render(
                PromptKey::SkillListingRow,
                &[("name", name), ("trigger", trigger)],
            )
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return schema;
    }
    let description = schema["description"].as_str().unwrap_or_default();
    schema["description"] = json!(format!(
        "{description}\n\n{}\n{}",
        profile.text(PromptKey::SkillListingHeading),
        lines.join("\n")
    ));
    schema
}

/// Run-specific `skill` schema. Entries are `(name, trigger)` pairs.
pub(crate) fn skill_schema_for_entries(
    entries: &[(String, String)],
    profile: &PromptProfile,
) -> Value {
    skill_schema(Some(entries), profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tool_catalog;
    use std::collections::BTreeSet;

    fn properties(schema: &Value) -> BTreeSet<String> {
        schema["properties"]
            .as_object()
            .expect("schema properties object")
            .keys()
            .cloned()
            .collect()
    }

    /// `playwright` and `todo` are discriminated unions, so their shape claims hold per
    /// variant. Every other tool has exactly one shape and is checked directly.
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
        assert_eq!(workflow["$defs"]["agentType"]["enum"], json!(["alpha", "zeta"]));
        // The script prose points to `$defs` rather than repeating role names.
        let description = workflow["properties"]["script"]["description"]
            .as_str()
            .expect("script description");
        assert!(description.contains("$defs.agentType"), "{description}");
        assert!(!description.contains("alpha"), "名字只该出现在 enum 里：{description}");
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

    /// Skill names belong in the machine-readable enum; triggers belong in description
    /// prose.
    #[test]
    fn skill_names_become_enum_values_and_triggers_become_description_lines() {
        let entries = vec![
            ("Commit Helper".to_owned(), "Use when writing a commit".to_owned()),
            ("PDF Tools".to_owned(), "Use when reading a PDF".to_owned()),
        ];

        let schema = skill_schema_for_entries(&entries, &PromptProfile::builtin_english());

        assert_eq!(
            schema["properties"]["name"]["enum"],
            json!(["Commit Helper", "PDF Tools"])
        );
        let description = schema["description"].as_str().expect("skill description");
        assert!(description.contains(PromptKey::SkillListingHeading.builtin_en()), "{description}");
        assert!(
            description.contains("- Commit Helper: Use when writing a commit"),
            "{description}"
        );
        assert!(
            description.contains("- PDF Tools: Use when reading a PDF"),
            "{description}"
        );
    }

    /// The static baseline has no enum and must not promise a skill listing it lacks.
    #[test]
    fn the_static_skill_baseline_carries_no_enum_and_promises_no_listing() {
        let schema = builtin_tool_schema("skill", &PromptProfile::builtin_english()).expect("skill has a static schema");

        assert!(schema["properties"]["name"].get("enum").is_none(), "{schema}");
        let description = schema["description"].as_str().expect("skill description");
        assert!(!description.contains(PromptKey::SkillListingHeading.builtin_en()), "{description}");
        // The static baseline must retain its complete property surface for golden and
        // catalog consistency comparisons.
        assert_eq!(schema["required"], json!(["name"]));
    }

    /// Omit the skill listing entirely when every trigger is blank.
    #[test]
    fn skills_without_a_trigger_contribute_no_listing_line() {
        let entries = vec![
            ("Silent".to_owned(), String::new()),
            ("Blank".to_owned(), "   ".to_owned()),
        ];

        let schema = skill_schema_for_entries(&entries, &PromptProfile::builtin_english());

        assert_eq!(schema["properties"]["name"]["enum"], json!(["Silent", "Blank"]));
        let description = schema["description"].as_str().expect("skill description");
        assert!(!description.contains(PromptKey::SkillListingHeading.builtin_en()), "{description}");
    }

    /// Required-role mode requires `agent_type` and removes `context` with its `not`
    /// guard because conversation context and named roles are mutually exclusive.
///
    #[test]
    fn requiring_a_role_puts_agent_type_in_required_and_removes_context() {
        let names = vec!["alpha".to_owned()];

        let required = agent_spawn_schema_for_roles(&names, true, &PromptProfile::builtin_english());
        assert_eq!(
            required["required"],
            json!(["prompt", "name", "agent_type"])
        );
        assert!(required["properties"].get("context").is_none(), "{required}");
        assert!(required.get("not").is_none(), "{required}");
        // Required mode retains the role enum; it narrows requiredness, not choices.
        assert_eq!(required["properties"]["agent_type"]["enum"], json!(["alpha"]));
        // Other properties remain.
        for key in ["prompt", "name", "label", "schema"] {
            assert!(required["properties"].get(key).is_some(), "{key}: {required}");
        }

        let fallback = agent_spawn_schema_for_roles(&names, false, &PromptProfile::builtin_english());
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
            workflow_schema_for_roles(&names, required, &PromptProfile::builtin_english())["properties"]["script"]["description"]
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

        let mut spawn = agent_spawn_schema_for_roles(&names, false, &PromptProfile::builtin_english());
        let spawn_base = spawn["description"].as_str().expect("spawn description").to_owned();
        append_role_descriptions(&mut spawn, &roles, &PromptProfile::builtin_english());
        let spawn_block = spawn["description"]
            .as_str()
            .expect("spawn description")
            .strip_prefix(&spawn_base)
            .expect("块必须追加在原描述之后")
            .to_owned();

        let mut workflow = workflow_schema_for_roles(&names, false, &PromptProfile::builtin_english());
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
        assert_eq!(spawn["properties"]["agent_type"]["enum"], json!(["reviewer"]));
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
        let description = workflow_schema(None, false, &PromptProfile::builtin_english())["properties"]["script"]["description"]
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
            let schema = builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).unwrap();
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
            let schema = builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).unwrap();
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

    /// One action per variant, all 23 of them, each discriminated by an `action` const that is
    /// also required. Without this the union would still validate but stop being a union the
    /// model can navigate.
    #[test]
    fn playwright_variants_cover_every_action_exactly_once() {
        let schema = builtin_tool_schema("playwright", &PromptProfile::builtin_english()).unwrap();
        let variants = schema["oneOf"].as_array().expect("playwright oneOf");
        let declared: Vec<String> = variants
            .iter()
            .map(|variant| {
                let action = variant["properties"]["action"]["const"]
                    .as_str()
                    .expect("action const")
                    .to_owned();
                let required: Vec<&str> = variant["required"]
                    .as_array()
                    .expect("variant required")
                    .iter()
                    .map(|entry| entry.as_str().expect("required entry"))
                    .collect();
                assert!(required.contains(&"action"), "{action} must require action");
                action
            })
            .collect();
        let expected: Vec<String> = crate::browser::PlaywrightAction::ALL
            .iter()
            .map(|action| action.as_str().to_owned())
            .collect();
        assert_eq!(declared, expected);
        assert_eq!(
            declared.iter().collect::<BTreeSet<_>>().len(),
            declared.len()
        );
    }

    #[test]
    fn required_entries_reference_declared_properties() {
        for tool in tool_catalog() {
            let schema = builtin_tool_schema(&tool.name, &PromptProfile::builtin_english()).unwrap();
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

    /// Same union contract as `playwright`, one tier down: the merged state tool must expose
    /// exactly the actions its executor dispatches on, each discriminated by a required
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
