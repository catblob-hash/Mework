# Working with Mework

This page walks through the parts of the app you touch every day, in the order you meet them. It states what the current build does; where a limit is deliberate, it says so.

## Workspaces and conversations

A **workspace** is a folder on disk. File tools are bounded to it, the shell starts in it, project memory and project instructions are read from its `.mework/` directory, and `hooks.json` / `tool-descriptions/` there are discovered for it. Add one with **Add workspace** in the sidebar (a native folder picker; the host only trusts folders you picked this way). A workspace without a folder is a temporary scratch area under the app-data directory.

A **conversation** ("task" in the sidebar) belongs to a workspace and carries its own settings, history and background tasks. Three ways to create one:

- The **+** on a workspace row uses, in order: that workspace's default preset, the workspace's last conversation settings, then the global default preset.
- **New task** at the top of the sidebar, `Ctrl+N` and the empty-state button always use the global default preset.
- **Fork** from a message's menu copies the history up to that message into a new conversation.

Conversation history is editable: you can insert or edit user messages and model replies, delete contexts, and branch. Edits are persisted on the host and the next request is rebuilt from the persisted history, never from what the renderer happens to show.

## Presets and conversation settings

A **preset** is a reusable template (Settings → **Conversation presets**). Applying a preset copies its fields into the conversation and then the two are unrelated: editing the preset never changes existing conversations, and editing a conversation never writes back. "Set as default" marks the preset new conversations start from.

Preset fields, which are also the fields of the per-conversation drawer (the sliders icon next to the composer):

| Field | Meaning |
|---|---|
| System prompt | The base of the system prompt, and the only base there is — Mework has no default of its own. Leave it **empty** and the assembled prompt starts at the capability sections (skills, MCP servers, hooks, the app-data line). |
| Enabled tools | Which of the 27 tools the model may call. Memory tools, `task_wait`, `task_list` and `skill` are derived from switches, not listed here. |
| Agent roles | Named subagent roles: which model they run on, which tools they get, which search backend they use, and a description the model sees. |
| Skills, MCP servers, Hooks | Direct selection from what is installed or discovered; a dangling id stays selected but inactive. |
| Tool descriptions | The prompt profile (tool-description file) for this conversation. The English built-in is the default. See [Prompt profiles](prompt-profiles.html). |
| Web search | Native provider search or one of the catalog providers, and the per-call search cap. |
| Memory | Two independent switches: global memory (`~/.mework`) and project memory (`<workspace>/.mework`). |
| Skill delivery | Off: selected skill bodies are pasted into the system prompt. On: the `skill` tool loads them on demand. |
| Security policy | `request_approval`, `allow_edits`, `plan` or `full_access` — see below. |
| Include app data directory | Conversation-only: exposes the absolute app-data path in the system prompt. |
| Reasoning effort | Conversation-only: passed to the provider when the model supports it. |

## Tools and approvals {#tools-and-approvals}

The 27 built-in tools, by group:

- **Filesystem** — `ls`, `grep`, `read`, `write`, `edit`, `find`. Bounded to the workspace and trusted roots; symlinks, junctions and `..` cannot escape; writes are atomic and return a diff.
- **Shell** — `powershell`, `bash`. Foreground or background (`run_in_background`); the command text is shown in full on the approval card.
- **Web** — `web_search`, `web_fetch`, and `playwright` (23 browser actions over the built-in browser).
- **Orchestration** — `agent_spawn`, `send_message`, `followup_task`, `task_wait`, `task_list`, `workflow`, `fork`, `skill`, `todo`, `ask_user`.
- **Memory** — `read/create/edit_global_memory`, `read/create/edit_project_memory`.

The **security level** decides which calls ask you first:

| Level | Reads in workspace | Writes in workspace | Reads outside | Writes outside | Subagents | Fork | Shell, web, workflow |
|---|---|---|---|---|---|---|---|
| `request_approval` | allowed | ask | ask | ask | ask | ask | ask |
| `allow_edits` | allowed | allowed | allowed | ask | allowed | ask | ask |
| `plan` | as Manual | refused | ask | refused | ask | ask | ask |
| `full_access` | allowed | allowed | allowed | allowed | allowed | ask | allowed |

### Plan mode

**Plan mode** is a read-only planning phase: workspace reads follow Manual, while every filesystem write or edit is refused rather than shown as an approval. Shell, web search and fetch, browser interaction, workflows, MCP calls and subagent spawning still ask as they do in Manual; screenshots remain available. The host supplies three derived tools automatically, never through the tool picker or presets: in Plan mode, `plan` reads or replaces the host-stored Markdown plan and `exit_plan_mode` submits it for review; outside Plan mode, `enter_plan_mode` asks to switch into it. Subagents inherit Plan mode as read-only and never receive these tools.

Calling `exit_plan_mode` opens an approval card and takes the message area directly to the plan preview without expanding the task container. The card offers **Yes, auto-accept edits**, **Yes, manually approve edits**, or **No, keep planning**; the last choice requires feedback and keeps the model in Plan mode. A saved plan appears as an **Implementation plan** row in the task container, and clicking it opens the Markdown plan page. Either yes choice switches the conversation in the same turn, respectively to Accept edits or Manual, and the composer immediately shows that new level.

Some confirmations cannot be turned off by any level or by a hook: recursive deletes that touch the root, home or system paths; writing global memory; MCP tools that declare `anthropic/requiresUserInteraction`; and taking over a browser page you logged into yourself.

An approval card shows the tool, the final arguments (after any hook rewrite), the risk level and the rule that fired. **Always allow** remembers the decision for that tool in that conversation, capped at the risk level of the card you answered; shell tools and `playwright` never get a standing allowance. Cards raised by background tasks survive the end of the turn and reappear after a reload.

`fork` always raises a non-blocking request card, including at Full access, and never creates a child automatically. The child starts with only its prompt unless the model explicitly passes `inherit_context: true`; approved and declined decisions remain in the task container but are never reported to the model.

## Run environments

The **Run location** chip above the composer decides where `bash` / `powershell` execute: this machine, a WSL distribution, or an SSH machine you registered. Each location can carry environment variables that are injected into those commands; the gear on a row edits them. File tools always work on the local workspace — in WSL the same files are visible through the automount, over SSH the remote is a separate world and the selector only ships commands there. Approvals are fingerprinted with the environment, so changing it invalidates a pending approval.

## Subagents, workflows and tasks

Everything that keeps running is a **task** and shows in the task panel with a stop button: subagents, workflow runs, background shell commands, terminals and browser tabs.

- `agent_spawn` starts a subagent that runs in the background (up to 8 at once). It gets the parent's tools minus orchestration, a copy of the conversation history only when asked (`context: "conversation"`), and the child addendum from the prompt profile. `send_message` queues a message; `followup_task` wakes an idle subagent with a new turn. `task_wait` blocks until the named tasks settle; anything not collected is delivered at the next round boundary as a `<task-notification>`.
- **Roles** (agent definitions on the preset or conversation) let the model pick a subagent by name; you decide the model, tools and search backend behind that name. When roles exist, `agent_spawn` requires one unless "allow roleless subagents" is on.
- `workflow` runs a JavaScript orchestration script — `agent()`, `parallel()`, `pipeline()`, `phase()`, `log()`, `args`, `budget` — in a private pool with a persisted journal. Runs survive restarts, can be resumed with `resume_run_id`, and can isolate steps in git worktrees. It is an ordinary tool, off by default.
- `todo` keeps a task list the model maintains; `ask_user` pauses the turn with a question card you answer in the composer.

## Web search and fetch

`web_search` has two backends: **native** (the conversation's own model runs its provider-side search in an isolated request with no client tools) and a **catalog provider** (Tavily, Exa, SearXNG, Jina, Firecrawl, Bocha, Zhipu, …) that the host calls itself. `web_fetch` always uses the catalog fetch provider configured under Settings → **Search providers**. Both are asynchronous tools: one approval covers every network action of the call, results come back as the tool result marked `untrustedWebContent`, and the model is told in the system prompt that web text is evidence, never instruction.

## The built-in browser

`playwright` drives the built-in browser through accessibility snapshots (`snapshot`), refs from those snapshots (`click`, `type`, `fill_form`, `select`, …), navigation, screenshots, console and network reads, and tab management. Every tab is a single-use, isolated WebView2 profile: no shared cookies, nothing imported from your daily browser, destroyed on close. If you log into a site in a tab yourself, the model has to ask before it can act on that page. Remote content can never reach the app's IPC surface.

## Long-term memory

Two tiers of plain Markdown, each with the same layout:

```text
~/.mework/                       <workspace>/.mework/
  MEWORK.md                        MEWORK.md        standing instructions (you write these)
  memory/MEMORY.md                 memory/MEMORY.md index (the host maintains it)
  memory/<topic>.md                memory/<topic>.md memory documents (the model writes these)
```

With a tier's switch on, its `MEWORK.md` and `MEMORY.md` are pasted into the request and its three tools become available; document bodies are fetched by name when needed. Global memory writes always ask for confirmation. Memory is low-priority context for the model, not policy: rules that must always hold belong in hooks or the security level.

Project instruction files (`MEWORK.md`, `AGENTS.md`, `CLAUDE.md` style files in the workspace) are also discovered at startup and injected as an untrusted file-context block, independently of the memory switches.

## Images

Drop or paste images into the composer; each gets an `[Image #N]` placeholder in your message that the model can refer to. Tool results that produce images (`read` on an image file, browser screenshots) get numbers too, and `playwright upload_image` can send one of them to a page. Images are stored content-addressed under the app-data directory.

## Git and terminals

The **Git review** panel shows status, diffs and branches of the workspace and can commit, push and open pull requests when you configure a GitHub write identity. A conversation can also work in a dedicated **worktree** (chip above the composer). The **terminal** panel opens a PowerShell you drive yourself; the model sees it only as a task it can wait on.

## Keyboard, appearance, dependencies

Settings → **Shortcuts** lists every command and lets you rebind them (zoom keys are fixed). **Appearance** covers theme, accent, fonts, zoom, message layout and custom CSS. **Environment dependencies** probes `git`, `node`, `python`, `rg`, `fd`, `gh`, `uv`, `bun` on your `PATH` — detection only, no installs.

## Data and reset

The settings document is versioned. Data from a newer or older schema is quarantined and rebuilt, never migrated silently. To start over during development:

```bash
npm run reset:data           # wipe app data (add --keys to also remove stored API keys)
```

Uninstalling the app does not delete `%APPDATA%\com.mework.app`, `%LOCALAPPDATA%\com.mework.app`, the Credential Manager entries or `~/.mework`.
