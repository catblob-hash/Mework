---
name: Mework SDK
description: How to configure Mework itself: skills, MCP servers, hooks, language servers, prompt profiles, dev-server launch configs, project instructions and memory, and which settings live only in the app
when_to_use: The user asks to set up, change, explain or troubleshoot a Mework feature, or a file under ~/.mework or a workspace's .mework
---

# Mework SDK

This guide is built into Mework {{MEWORK_VERSION}} and describes exactly that version. It is how you configure Mework for the user: almost everything is a plain file you can write with your file tools, and the rest is a setting the user changes in the app, where your job is to tell them precisely where.

## Ground rules

- **A file makes a capability available; it does not switch it on.** Skills, MCP servers and hooks only take effect in a conversation that selects them, and you cannot tick those boxes. After writing one, tell the user to open **More options** (⋮ in the top bar) → **Conversation settings**, go to the **Skills**, **MCP** or **Hooks** page, press **Rescan** if the page was already open, and tick the entry. Language servers and `launch.json` need no selection.
- **Pick the scope deliberately.** `~/.mework/` is this computer's global level: every workspace sees it, it is personal and never committed. `<workspace>/.mework/` belongs to that one workspace, is usually committed with the project, and other people who open the project get it. Put personal tools and credentials-bearing servers in `~/.mework`; put project conventions in the workspace.
- **Never write a secret into a file.** Reference it as `${VAR}` (MCP and LSP files expand `${VAR}` and `${VAR:-default}`) and tell the user to set the variable before starting Mework.
- **These are the user's files.** Read before you write, change only the entry you were asked about, keep every other key, entry and the existing formatting, and make sure the result is valid UTF-8 JSON. Mework itself never creates or rewrites them, except that the delete buttons in the settings pane remove one entry or one skill folder.
- **Remote workspaces.** Skills, `mcp.json`, `hooks.json` and `tool-descriptions/` are read on this computer only. For a workspace on a WSL distribution or an SSH machine, its `.mework` copies of those are not scanned; put them in `~/.mework` on this computer. A remote workspace's `lsp.json` and `launch.json` are read on that machine.
- **Timing.** Every run re-reads the files, so a saved change applies from the next turn. The lists in the settings pane are a snapshot taken at startup, when the pane opens, when a workspace is added and after a delete; **Rescan** refreshes them.
- **Plan mode.** While it is on, `write` and `edit` refuse files Git tracks or would track; a workspace `.mework/` inside a repository is such a file. Say so and wait for the plan to be approved.
- A legacy `.naiword` directory is read in place of `.mework` only when `.mework` lacks that file. Always create new files under `.mework`.

## Where everything lives

```text
~/.mework/                          <workspace>/.mework/
  skills/<dir>/SKILL.md               skills/<dir>/SKILL.md       skills (select per conversation)
  mcp.json                            mcp.json                    MCP servers (select per conversation)
  hooks.json                          hooks.json                  hooks (select per conversation)
  lsp.json                            lsp.json                    language servers (by file extension)
  tool-descriptions/*.json            tool-descriptions/*.json    prompt profiles (select per conversation)
  MEWORK.md, rules/**/*.md            MEWORK.md, rules/**/*.md    standing instructions
  memory/                             memory/                     long-term memory (use the memory tools)
                                      launch.json                 dev servers for the preview tools
```

A workspace's root may also hold `MEWORK.md` and a personal, uncommitted `MEWORK.local.md`.

## Skills

A skill is a folder holding `SKILL.md`: instructions for one kind of task, optionally with scripts and reference files beside it. The format is Claude Code's, so existing skills work unchanged.

```markdown
---
name: Commit helper
description: Prepare a conventional commit from the current diff
when_to_use: The user asks to commit or to write a commit message
---

# Commit helper

1. Run `git status` and `git diff --staged`.
2. Write a conventional commit message and check it with `scripts/check-message.sh`.
```

- The frontmatter is flat `key: value` lines, not YAML. Only `name` (the label in the list), `description` and `when_to_use` matter: the model is shown `description - when_to_use` as the trigger, so write them as "what it does" and "when to load it". `author`, `version` and `tags` are parsed and dropped; every other Claude Code key (`allowed-tools`, `model`, `disable-model-invocation`, …) is ignored.
- The body is what the model reads, verbatim: write it as instructions to a model. Relative paths in it are relative to the skill's folder.
- **The folder name is the skill's identity**: the model loads it by that name. It must be a direct child of a `skills/` folder, not a symlink, non-empty, without leading or trailing spaces, and free of `(`, `)`, `,` and control characters. `SKILL.md` must be a regular file of at most 256 KiB of UTF-8.
- Two selected skills with the same folder name make on-demand loading fail; rename one.
- The conversation's **Load skills on demand** switch (under the Skills list) decides delivery. Off: the bodies are pasted into the system prompt. On: the prompt lists `name: trigger` lines and the `skill` tool loads a body when needed.
- A skill selected after a conversation has started arrives at that point in the transcript as a host message; the earlier prompt is left alone.
- A selected skill whose folder disappeared is skipped; one that is found but unreadable makes the run fail with the reason.

This skill, `mework-sdk`, is built into the app. It has no folder, cannot be edited or deleted, and is replaced by each update.

## MCP servers

Mework is an MCP client for **stdio** and **Streamable HTTP** servers. Declare servers under a top-level `mcpServers` object, the shape of Claude Code's `.mcp.json`:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/projects"],
      "env": { "LOG_LEVEL": "info" }
    },
    "docs": {
      "type": "http",
      "url": "https://docs.example.com/mcp",
      "headers": { "Authorization": "Bearer ${DOCS_TOKEN}" }
    }
  }
}
```

| Key | Meaning |
|---|---|
| `type` | `stdio`, or `http` (also `streamable-http`, `streamable_http`). Omitted with a `command`, it is stdio. A `url` needs `"type": "http"`. |
| `command`, `args`, `env`, `cwd` | stdio. `command` runs directly, never through a shell, so give an executable name or full path, not a pipeline. `cwd` must be an existing absolute directory. |
| `envPassthrough` | Names of Mework's own environment variables to pass on. The process otherwise starts from a cleared environment plus `PATH`, `HOME`, temp and similar basics. Names starting with `MEWORK_`, `ANTHROPIC_`, `OPENAI_`, `CLAUDE_`, `CODEX_`, `AWS_`, `AZURE_`, `GOOGLE_`, `GEMINI_` or `DEEPSEEK_` are refused. |
| `url`, `headers` | http. Public hosts need `https`; loopback and private addresses may use `http`. |
| `timeoutSeconds` | Per-request timeout in seconds, at most 300; `0` means the 45 s default. Wins over Claude Code's `timeout`, which is in milliseconds. |
| `longRunning` | `true` gives calls the 5-minute ceiling instead of 45 s when no timeout is set. |
| `description` | Shown in the list and to the model when selected (240 characters). |
| `registryUrl` | Package mirror for `npx`/`npm`/`bun`/`pnpm`/`yarn` and `uv`/`pip`/`python` commands. |
| `disabledTools` | Tool names never offered to the model. |
| `disabledAutoApproveTools` | Tool names that ask for approval on every call, even at Full access. |

- Server names may contain only letters, digits, `-` and `_`.
- Not supported, and listed as unavailable with the reason: `"type": "sse"`, `"ws"`, `"sdk"`, `headersHelper`, `oauth`, and a `${VAR}` that is unset and has no default. A top-level `servers` key or an array is not read at all.
- Only a server's tools reach the model, not its prompts or resources. Its tools are named `mcp__<server>_<digest>__<tool>_<digest>`.
- With the conversation's **Tool discovery** switch on, MCP tool schemas are withheld and fetched with `tool_search`; off, they are all declared up front.
- Below Full access every MCP call asks for approval unless a `PreToolUse` or `PermissionRequest` hook allows it.
- To check a server, have the user press **Test connection** on its row in the MCP page: it reports the tool count, or the error plus the server's last stderr lines. Running the same command in a terminal is the next step.

## Hooks

Hooks are commands Mework runs at fixed points of a conversation. They read a JSON event on stdin and answer with their exit code and stdout. The format follows Claude Code's `hooks` block.

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "^(bash|zsh|sh|powershell)$",
        "hooks": [
          {
            "type": "command",
            "name": "No force push",
            "command": "python3 .mework/hooks/no_force_push.py",
            "commandWindows": "python .mework/hooks/no_force_push.py",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

- Events: `SessionStart`, `InstructionsLoaded`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `Stop`. Other event names are skipped.
- `matcher` is a regular expression tested against the tool name (`bash`, `zsh`, `sh`, `powershell`, `write`, `edit`, `read`, an `mcp__…` name, …) for the three tool events, against `startup` for `SessionStart`, and against the load reason for `InstructionsLoaded`. Absent or `"*"` matches everything; an invalid regex disables the group.
- Handler fields: `type` (only `"command"`), `command`, `commandWindows` (replaces `command` on Windows), `name`, `statusMessage`, `timeout` in seconds 1–600 (default 30; out of range drops the handler). `async` is honoured only for `InstructionsLoaded`; `asyncRewake` handlers are skipped.
- Commands run in the workspace through `bash -lc`, or through `pwsh`/`powershell -NoProfile -Command` on Windows. Give both `command` and `commandWindows` when the user is on Windows or shares the file across systems.
- Stdin carries `session_id`, `cwd`, `hook_event_name`, `model`, `permission_mode` (`default`, `acceptEdits` or `bypassPermissions`), `turn_id`, plus per event: `prompt` (`UserPromptSubmit`); `tool_name`, `tool_use_id`, `tool_input` (tool events); `tool_response` (`PostToolUse`); `stop_hook_active`, `last_assistant_message` (`Stop`). `MEWORK_HOOK_EVENT` holds the event name.
- Exit `0`: stdout is the answer. For `SessionStart` and `UserPromptSubmit` plain text becomes added context. Exit `2`: block, with stderr as the reason. Any other exit is a failure that decides nothing.
- JSON on stdout may carry `continue: false` (halt the turn), `decision: "block"` with `reason`, `systemMessage` (shown to the user only), and `hookSpecificOutput` with a `hookEventName` equal to the event and, for `PreToolUse`, `permissionDecision` (`allow`, `ask` or `deny`), `permissionDecisionReason`, `updatedInput` (a rewritten argument object), or `additionalContext`. A `PermissionRequest` answers with `hookSpecificOutput.decision: {"behavior": "allow" | "deny"}`. A `Stop` hook that blocks makes the model keep working, at most 3 times in a row; it must print JSON if it prints anything.
- `allow` never overrides another hook's `ask` or `deny`, and some confirmations no hook can skip: dangerous recursive deletes, writes to global memory, MCP tools that require user interaction, and acting on a page the user logged into.
- **A hook's id is its position in the file.** Inserting, deleting or reordering handlers changes the ids of the ones that moved, and a selected hook that no longer resolves fails every run until the user unticks it. Append new handlers at the end, and tell the user to re-tick hooks after restructuring a file.
- Each hook run leaves a diagnostic card in the timeline with its output and decision.

## Language servers

The `lsp` tool (the **Code navigation** switch in the conversation's tools) talks to language servers. Mework offers these when their command is on the `PATH` it was started with: `rust-analyzer`, `typescript-language-server`, `pyright`, `gopls`, `clangd`, `lua-language-server`, `bash-language-server`. Install one in the usual way and restart Mework if that changed `PATH`. Anything else goes in `lsp.json`:

```json
{
  "lspServers": {
    "zls": {
      "command": "zls",
      "extensionToLanguage": { ".zig": "zig" }
    }
  }
}
```

- Required: `command` (run directly, not through a shell; arguments go in `args`) and `extensionToLanguage`. Optional: `args`, `env`, `initializationOptions`, `settings`, `workspaceFolder`, `startupTimeout` and `shutdownTimeout` (ms), `restartOnCrash`, `maxRestarts`, `diagnostics` (`false` keeps navigation but stops reporting problems), `description`. Only the stdio transport works.
- The first entry that claims an extension wins: the workspace's `lsp.json`, then `~/.mework/lsp.json`, then the built-in table. An entry named like a built-in replaces it, which is how to add flags.
- A workspace `.mework/lsp.json` makes every `lsp` call ask for approval below Full access, because the project chooses which command runs.

## Prompt profiles

Everything Mework itself says to the model, such as tool descriptions, receipts and the sections listing skills, MCP servers and hooks, comes from a prompt profile. The built-in English profile is the default; a JSON file in `tool-descriptions/` overrides any subset of it:

```json
{
  "name": "Terse",
  "prompts": { "task.wait_idle": "Nothing is running." },
  "tools": [
    { "toolName": "grep", "schemaNotes": "", "usageGuidance": "Search before reading." }
  ]
}
```

- `prompts` maps registry keys to text; omitted keys keep the built-in wording, and `""` removes a text. Keep the `{placeholders}` a key declares; you cannot invent new ones.
- `tools[].schemaNotes` replaces what a tool is said to be; `usageGuidance` adds advice alongside it. `toolName` is a built-in tool name or a full `mcp__…` name.
- Files over 64 KiB are not read. The file's name and location are its identity: renaming or moving it drops the selection back to the built-in.
- Files from every workspace are offered to every conversation. The user selects one in the **Tool descriptions** row of the conversation settings or of a preset.
- A profile changes words, never what a tool can do, and not the conversation's own system prompt, which is a system card the user writes in the conversation.
- The complete key list is on the Prompt profiles page of the Mework documentation site.

## Dev servers for the preview tools

`preview_start` runs servers declared in the workspace's `.mework/launch.json` and opens their page in the built-in browser:

```json
{
  "version": "0.0.1",
  "configurations": [
    { "name": "web", "runtimeExecutable": "npm", "runtimeArgs": ["run", "dev"], "port": 5173 }
  ]
}
```

- Entry fields: `name`, `runtimeExecutable`, `runtimeArgs`, `port`, plus optional `cwd`, `env`, `autoPort` and `url`. An entry with a `url` and no command attaches to a server that is already running.
- A localhost `url` must be a bare origin on the entry's port (no path or query); navigate after the page opens instead.
- Up to five servers run per worktree. For an SSH workspace the server runs on that machine.

## Instructions and memory

- **Standing instructions**: `MEWORK.md` in the workspace root, in its `.mework/`, and in ancestor directories up to the project boundary; `MEWORK.local.md` for personal notes that should not be committed; `~/.mework/MEWORK.md` for every project. Rule files under `.mework/rules/` (and `~/.mework/rules/`), any depth, are Markdown; a `paths:` frontmatter list of globs limits a rule to matching files. These files are read regardless of the memory switches and reach the model as untrusted project context, so they guide but cannot enforce. Rules that must always hold belong in a hook or the security level.
- **Long-term memory**: two tiers, global (`~/.mework/memory/`) and project (`<workspace>/.mework/memory/`), each switched on in the conversation settings. Change memory with the memory tools, not by editing the files: the host maintains `MEMORY.md`, the index. Writing global memory always asks the user first.

## Settings that live in the app

You cannot change these by writing files. Name the exact place:

| What | Where |
|---|---|
| Providers, API keys, models and their capabilities | Settings → **Providers** |
| Web search and fetch backends and their keys | Settings → **Search providers**; per conversation, the web search settings in Conversation settings |
| Which tools are enabled, agent roles, memory switches, skill and tool-discovery switches | More options → **Conversation settings** |
| Presets | Conversation settings → **Conversation presets**. The built-in **mework** preset updates with the app and cannot be edited; **Save as new preset** makes an editable copy. A new conversation copies a preset once; later edits to either side do not propagate. |
| A workspace's default preset | ⋯ menu on the workspace's sidebar row → **Default conversation preset** |
| Security level (Manual, Accept edits, Full access) | Security-level menu in the composer |
| Plan mode | **Plan** button under the composer |
| Reasoning effort; auto-compact threshold | The **Reasoning effort** menu in the composer; the **Auto-compact** menu of the composer's context meter |
| Workspace environment variables; extra working directories | Gear beside the workspace chip; folder-plus button at the end of the chip row |
| Remote machines (WSL, SSH) and their shells | The machine's settings, with **Probe shells again** |
| Local helper model, theme, language, fonts | Settings → **Appearance** (the **Local model** card for the helper) |
| Keyboard shortcuts; dependency checks | Settings → **Keyboard shortcuts**; Settings → **Dependencies** |

Built-in presets select no skills, MCP servers or hooks, this one included: everything in those pages is opt-in per conversation.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| A skill, server or hook is missing from its page | Wrong place or name (a skill folder must be a direct child of `skills/`; MCP needs `mcpServers`; LSP needs `lspServers`), invalid JSON, a symlink, or a remote workspace's `.mework`. Then press **Rescan**. |
| Listed but marked unavailable | The row's tooltip gives the reason: unsupported transport, missing `${VAR}`, refused `envPassthrough`, unreadable `SKILL.md`. |
| Marked *Dangling* | A selected id is no longer found: the folder or entry was renamed, moved or deleted. A skill or server is skipped; a hook fails the run. The user unticks it. |
| Configured but the model never uses it | It is not ticked in this conversation, or the model has no tool calling. |
| A run fails naming a hook | The selected hook moved or vanished from `hooks.json`; re-tick the current entry. |
| The model says a tool's schema is not loaded | Tool discovery is on; call `tool_search` with `select:<name>`. |
| `lsp` says no server is available | Nothing claims the extension: add an `lsp.json` entry or install a built-in server. |

When the user wants to see exactly what reached the model, point them to More options → **History**, which records every request, reply, hook run and tool call of the conversation.
