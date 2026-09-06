# Hooks

Hooks are commands Mework runs at fixed points of a conversation's lifecycle. They read a JSON description of the event on **stdin** and answer on **stdout** (and with their exit code). A hook can add context, block a prompt or a tool call, rewrite a tool call's arguments, allow or deny a permission request, or ask the model to keep working when it tries to stop. The file format and the event vocabulary follow Claude Code's hooks, so existing `hooks.json` files carry over.

## Where hooks are defined

Hooks are **not** edited in the app. Mework reads two files and lists what it finds:

```text
~/.mework/hooks.json                 user scope — every workspace
<workspace>/.mework/hooks.json       workspace scope — that workspace only
```

(`~/.naiword/hooks.json` is still read when the `.mework` file does not exist, for older setups.) Edit the file, then open the conversation drawer: the **Hooks** section shows every handler with its event as the description. Each handler is one selectable entry; a conversation (or a preset) selects the handlers it wants under `hookIds`. Nothing runs until it is selected.

## File format

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "name": "Branch context",
            "command": "printf 'branch: %s' \"$(git branch --show-current)\"",
            "commandWindows": "Write-Output \"branch: $(git branch --show-current)\"",
            "timeout": 10
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "^(bash|powershell)$",
        "hooks": [
          { "type": "command", "name": "No force push", "command": "python hooks/no_force_push.py" }
        ]
      }
    ]
  }
}
```

| Field | Meaning |
|---|---|
| `hooks.<Event>` | An array of **groups** for that event. Event names are exactly `SessionStart`, `InstructionsLoaded`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `Stop`. |
| `matcher` | Optional regular expression tested against the event's subject: the tool name for `PreToolUse` / `PermissionRequest` / `PostToolUse`, the source (`startup`) for `SessionStart`. `UserPromptSubmit` and `Stop` ignore it. `"*"` or an absent matcher matches everything; an invalid regex disables the group. |
| `hooks[]` | The handlers of the group. Only `"type": "command"` is supported. |
| `command` | The command line. On Windows it runs through `pwsh` (or `powershell`) `-NoProfile -Command`; elsewhere through `bash -lc`. |
| `commandWindows` | Optional Windows-only command line that replaces `command` on Windows, so one file can serve both shells. |
| `name` | Display name; defaults to `statusMessage`, then `<Event> #<n>`. |
| `statusMessage` | Shown in the timeline while the hook runs. |
| `timeout` | Seconds, 1–600 (default 30). |

Handlers marked `async: true` are accepted only for `InstructionsLoaded`; `asyncRewake` is not supported and such handlers are skipped.

## What a hook receives

The command's working directory is the workspace. Stdin is a UTF-8 JSON object; the environment variable `MEWORK_HOOK_EVENT` also carries the event name. Common fields:

```json
{
  "session_id": "<conversation id>",
  "transcript_path": null,
  "cwd": "<workspace path>",
  "hook_event_name": "PreToolUse",
  "model": "<model id>",
  "permission_mode": "default | acceptEdits | bypassPermissions",
  "turn_id": "<uuid>"
}
```

`permission_mode` maps the conversation's security level: `request_approval` → `default`, `allow_edits` → `acceptEdits`, `full_access` → `bypassPermissions`. Event-specific fields:

| Event | Extra fields |
|---|---|
| `SessionStart` | `source: "startup"` — fires once per conversation, on its first turn in this app session. |
| `UserPromptSubmit` | `prompt` — the last real user message. |
| `PreToolUse` | `tool_name`, `tool_use_id`, `tool_input` |
| `PermissionRequest` | `tool_name`, `tool_input`, `permission_suggestions: []` — fires only for calls that would otherwise show an approval card. |
| `PostToolUse` | `tool_name`, `tool_use_id`, `tool_input`, `tool_response` (the public projection of the result) |
| `Stop` | `stop_hook_active` (true when the model is continuing because of an earlier Stop hook), `last_assistant_message` |
| `InstructionsLoaded` | `file_path`, `memory_type` (`User` / `Project` / `Local` / `Managed`), `load_reason` (`session_start` / `nested_traversal` / `path_glob_match` / `include`), and when relevant `globs`, `trigger_file_path`, `parent_file_path`. No instruction body. |

Tool hooks also run for subagents and workflow steps; `SessionStart`, `UserPromptSubmit` and `Stop` belong to the visible conversation only.

## How a hook answers

**Exit code** first:

- `0` — success; stdout is interpreted as below.
- `2` — **block**. Stderr becomes the reason shown to the model (or `hook.blocked_default` from the prompt profile when stderr is empty). For `PreToolUse` and `PermissionRequest` this denies the call; for `UserPromptSubmit` and `SessionStart` it ends the turn before any model request; for `PostToolUse` it rejects the result; for `Stop` it blocks the *stop* — the model is given the reason and keeps working, exactly like `decision: "block"` below.
- anything else — the hook failed; it produces no decision and the turn continues (the failure is recorded in the timeline).

Then **stdout**. For `SessionStart` and `UserPromptSubmit`, plain text is accepted and injected as additional context. For every event, a JSON object is parsed with these fields:

```json
{
  "continue": false,
  "stopReason": "…",
  "decision": "block",
  "reason": "…",
  "systemMessage": "…",
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow | ask | deny",
    "permissionDecisionReason": "…",
    "updatedInput": { "command": "git push --force-with-lease" },
    "additionalContext": "…"
  }
}
```

| Field | Effect |
|---|---|
| `continue: false` | Halts the turn (any event). `stopReason` or `reason` is recorded. |
| `decision: "block"` + `reason` | Blocks the event. On `Stop`, a block **without** `continue: false` means "keep working": the reason (or `hook.continue_fallback`) is sent to the model as a new user message, at most 3 times in a row (`hook.stop_limit_reached`). |
| `systemMessage` | Shown in the timeline only; the model does not see it. |
| `hookSpecificOutput.hookEventName` | Must equal the current event, otherwise the specific output is ignored. |
| `permissionDecision` (`PreToolUse`) | `allow` skips the approval card for this call; `ask` forces a card even at `full_access`; `deny` blocks with `permissionDecisionReason`; `defer` is accepted but ignored. Legacy top-level `decision: "approve"` / `"block"` also work here. |
| `updatedInput` (`PreToolUse`) | Replaces the call's arguments **before** classification: the rewritten call is re-classified and approved on its own terms. A non-object value is ignored and the original call proceeds through the normal permission flow. |
| `PermissionRequest` decision | Nested: `hookSpecificOutput.decision = { "behavior": "allow" \| "deny", "updatedInput": {...}, "interrupt": true }`. `updatedPermissions` is accepted but never persisted — Mework grants for the current call only. |
| `additionalContext` | Injected as a non-local system context the model sees on the next request. |

Stop hooks must answer with JSON if they print anything at all. Hook output is capped at 64 KiB; all synchronous hook work in one turn shares a 5-minute budget on top of the per-handler timeout.

Two guarantees that follow from "classify what will actually run": an `allow` never overrides another hook's `ask` or `deny`, and a call denied before execution never triggers `PostToolUse`. `PostToolUse` cannot undo a tool that already ran (a file write is on disk); its rejection only changes what the model is told, and the receipt says so (`hook.post_tool_not_rolled_back`). The mandatory confirmations listed under [approvals](working.html#tools-and-approvals) cannot be pre-approved by a hook.

`InstructionsLoaded` is observation only: its exit code, stdout and JSON are recorded and never reach the model.

## Examples

Block dangerous git commands (`PreToolUse`, matcher `^(bash|powershell)$`), in Python:

```python
import json, sys, re
event = json.load(sys.stdin)
command = event.get("tool_input", {}).get("command", "")
if re.search(r"git\s+push\s+.*--force\b(?!-with-lease)", command):
    print("force pushes are not allowed; use --force-with-lease", file=sys.stderr)
    sys.exit(2)
```

Rewrite instead of blocking (same event), answering with JSON on stdout:

```python
import json, sys
event = json.load(sys.stdin)
tool_input = event["tool_input"]
tool_input["command"] = tool_input["command"].replace("--force", "--force-with-lease")
print(json.dumps({
    "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "ask",
        "updatedInput": tool_input,
        "permissionDecisionReason": "rewrote --force to --force-with-lease"
    }
}))
```

Add the current branch to every prompt (`UserPromptSubmit`, plain-text stdout):

```json
{ "type": "command", "name": "Branch", "command": "git branch --show-current", "commandWindows": "git branch --show-current" }
```

Keep the model working until tests pass (`Stop`):

```python
import json, subprocess, sys
event = json.load(sys.stdin)
if event.get("stop_hook_active"):
    print(json.dumps({}))          # already continuing once; let it stop
    sys.exit(0)
if subprocess.run(["npm", "test"], capture_output=True).returncode != 0:
    print(json.dumps({"decision": "block", "reason": "npm test fails; fix it before finishing"}))
else:
    print(json.dumps({}))
```

## Where to see what happened

Every hook run leaves a diagnostic card in the timeline: event, status, name, duration, stdout/stderr and the decision. These cards are local-only and never enter a model request. What the model does see is the reason on a blocked call's result, injected `additionalContext`, and the continuation message of a Stop hook — all worded through the [prompt profile](prompt-profiles.html) keys that start with `hook.`.

The selected hooks are also listed in the system prompt (`system.hooks_section`), with a sentence telling the model that hooks run on the host and that it must not claim a hook succeeded.
