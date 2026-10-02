# Working with Mework

This page walks through the parts of the app you touch every day, in the order you meet them. It states what the current build does; where a limit is deliberate, it says so.

## Workspaces and conversations

A **workspace** is a folder on disk. File tools are bounded to it, the shell starts in it, project memory and project instructions are read from its `.mework/` directory, and `hooks.json`, `mcp.json`, `lsp.json`, `skills/` and `tool-descriptions/` there are discovered for it. Add one with **Add workspace** in the sidebar (a native folder picker; the host only trusts folders you picked this way). A workspace without a folder is a temporary scratch area under the app-data directory.

A **conversation** ("task" in the sidebar) belongs to a workspace and carries its own settings, history and background tasks. Three ways to create one:

- The **+** on a workspace row uses, in order: that workspace's default preset, the workspace's last conversation settings, then the global default preset.
- **New task** at the top of the sidebar, `Ctrl+N` and the empty-state button always use the global default preset.
- **Branch from this message**, on the toolbar of any user message, copies the history before it into a new conversation in the same project and hands that message to the new composer as a draft.

Conversation history is editable: you can insert or edit user messages and model replies, delete contexts, and branch. Edits are persisted on the host and the next request is rebuilt from the persisted history, never from what the renderer happens to show.

## Presets and conversation settings

A **preset** is a reusable copy of a conversation's settings (conversation settings → **Conversation presets**). Applying a preset copies its fields into the conversation and then the two are unrelated: editing the preset never changes existing conversations, and editing a conversation never writes back. **Save as preset** stores the current conversation's settings under a new name; the opening messages of a preset are written on the preset's own **Conversation template** page, not captured from the conversation. A workspace names the preset its **+** uses under **Default conversation preset** in the ⋯ menu on its sidebar row.

Mework ships one preset, **mework**, and it is built in: it updates with each version of Mework and cannot be renamed, edited or deleted. Its window still lets you change any setting, and **Save as new preset** keeps the result as a preset of your own; its conversation template is shown read-only and does not go with the copy.

Preset fields, most of them edited in the conversation settings pane (More options → **Conversation settings**, `Ctrl+Shift+,`):

| Field | Meaning |
|---|---|
| Enabled tools | Which of the {{SWITCHABLE_TOOL_COUNT}} switchable tools the model may call. The remaining {{DERIVED_TOOL_COUNT}} of the {{TOOL_COUNT}} in the catalog are derived from other settings or tools and never appear here: the six memory tools, `task_wait`, `task_list`, `box`, `web_search`, `web_fetch`, `skill`, `tool_search`, `plan` and `exit_plan_mode`, and `preview_start`, `preview_stop` and `preview_list`, which are on whenever any other preview tool is. See [Tools](tools.html). |
| Agent roles | Named subagent roles: which model they run on, which tools they get, which search backend they use, and a description the model sees. **Allow role-less subagents** sits with them. |
| Skills, MCP servers, Hooks | Selected per conversation from those discovered in the global `~/.mework` plus this workspace's `.mework`. A dangling skill or MCP server stays selected and is skipped at run time; a hook id that is no longer found fails the run until you uncheck it. |
| Tool descriptions | The prompt profile (tool-description file) for this conversation. The built-in is the default. See [Prompt profiles](prompt-profiles.html). |
| Web search | One **Enable web search** switch, then a search backend and a fetch backend chosen separately (native provider search or a catalog provider, either one off on its own), a result count, a result-compression cutoff and a domain allowlist or blocklist. |
| Memory | Two independent switches: global memory (`~/.mework`) and project memory (`<workspace>/.mework`). |
| Load skills on demand | Off: selected skill bodies are pasted into the system prompt. On: the `skill` tool loads them on demand. |
| Tool discovery | Off: every selected MCP server's tool schemas are declared up front. On: only names go out and `tool_search` fetches the schemas a run held back. |
| Security level | Manual (`request_approval`), Accept edits (`allow_edits`) or Full access (`full_access`) — chosen from the security-level menu in the composer at any time, a running turn included, and saved with the preset. See below. |
| Conversation template | Preset only: the opening messages laid into a conversation when the preset is applied, a system prompt among them. Mework has no system prompt of its own — with none written, the assembled prompt starts at the host sections: the environment block, then skills, MCP servers, hooks and the app-data line. A live conversation has no template page; its timeline is its own queue, editable in place. |
| Include app data directory | Conversation-only: exposes the absolute app-data path in the system prompt. |
| Reasoning effort | Conversation-only: one of five levels, `low`, `medium`, `high`, `extra` and `max` (there is no "off"). Each reaches the provider as that model's own control; a level the model lacks drops to the highest one it has below it. |

## Tools and approvals {#tools-and-approvals}

The {{TOOL_COUNT}} built-in tools, by group — each has its own page under [Tools](tools.html):

- **Filesystem** — `ls`, `grep`, `read`, `write`, `edit`, `find` (the picker's [`files`](tools.html#files) row) and `lsp`. Bounded to the workspace and trusted roots; symlinks, junctions and `..` cannot escape; writes are atomic and return a diff. Five write guards are always on, for every conversation and every subagent: `edit`/`write` on an existing file require a prior read, a write stale against the file's modification time is refused unless it is an `edit` whose search text still matches exactly once, external changes to files you have read are reported at the next round, a hook (formatter) rewrite resyncs the host's record of the file, and a shell formatter command that touched files you have read is flagged. `lsp` answers from a language server (see [Code navigation](lsp.html)); a server the workspace's own `.mework/lsp.json` names asks for approval like a command, one from `~/.mework/lsp.json` or a built-in preset does not.
- **Shell** — `bash`, `zsh`, `sh`, `powershell` (the picker's [`shell`](tools.html#shell) row): one tool per shell backend, listed when some machine in the conversation has that shell. Foreground or background (`run_in_background`); the command itself is what the approval card shows, collapsed to one line.
- **Web** — `web_search`, `web_fetch`, and fifteen `preview_*` tools (the picker's [`preview`](tools.html#preview) row) that run the project's dev server and act on the page it serves.
- **Orchestration** — `agent_spawn`, `task_wait`, `task_list`, `box`, `workflow`, `fork`, `skill`, `tool_search`, `ask_user`, `plan`, `exit_plan_mode`.
- **Memory** — `read/create/edit_global_memory`, `read/create/edit_project_memory`.

The **security level** decides which calls ask you first:

| Level | Reads in workspace | Writes in workspace | Reads outside | Writes outside | Subagents | Fork | Shell, web, browser, workflow |
|---|---|---|---|---|---|---|---|
| `request_approval` (Manual) | allowed | ask | ask | ask | ask | ask | ask |
| `allow_edits` (Accept edits) | allowed | allowed | allowed | ask | allowed | ask | ask |
| `full_access` (Full access) | allowed | allowed | allowed | allowed | allowed | ask | allowed |

A new level takes effect at once, in the middle of a streaming turn and while an approval card is waiting included: the running turn and its subagents check every later call against it.

### Plan mode {#plan-mode}

**Plan mode** is the **Plan** button under the composer, beside the security level — a switch of its own, not a level, and not part of a preset: a new conversation starts with it off. Turn it on at any time, a running turn included, and from the next round on the model is asked to research, write a plan and get it approved before it changes anything.

- **Guidance, appended each time.** Every time you turn it on, the plan-mode instructions are appended to the conversation at that point, as a system message — a mid-conversation system message on the models with the **Mid-conversation system prompts** capability (in the model's properties under **Settings → Providers**; Mework ticks it itself for Anthropic's own API on Claude Opus 4.8 and later, Sonnet 5.5, Fable and Mythos, for OpenAI Responses, Azure and Codex, and for OpenAI Chat at OpenAI's endpoint, and leaves it to you for a relay or another Chat endpoint), and as a host message in `box` everywhere else, the way a background result arrives. Either way it is appended, so the prompt cache of everything before it stays intact. The two carriers are not interchangeable the other way round: a neutral host message never becomes a system message, which would give it the weight of an instruction. Turning it off before a plan is approved appends a short note that plan mode ended. The text of both is a [prompt profile](prompt-profiles.html) key, `system.plan_mode` and `system.plan_mode_exit`.
- **Tools, once.** The first time, the model gets two tools, never through the tool picker: `plan` reads or replaces the host-stored Markdown plan, and `exit_plan_mode` asks you to approve it. They stay for the rest of the conversation, so turning plan mode on again adds nothing to the tool list. On a model that cannot take a tool mid-conversation, the button is disabled once the first request has gone out without them.
- **One way out.** The instructions ask the model to end its turn by calling `exit_plan_mode`. That is the only thing that asks you to approve a plan: a turn that ends any other way just ends, with nothing put to you on the model's behalf, and plan mode stays on. `ask_user` is an ordinary tool call — your answer is its result and the turn goes on.

While plan mode is on, `write` and `edit` leave every Git repository's content alone: a call whose target Git tracks, or a new file inside a work tree that Git does not ignore, is refused with a reply telling the model the change waits until plan mode ends. Ignored paths and everything outside a repository stay writable, and nothing else is checked — shell commands are left to the instructions. The rule follows the switch as it stands, on the conversation's own machines (WSL and SSH workspaces included), and a call you run again by hand from the timeline is not subject to it. Otherwise the security level decides what every call needs, as it always does. Subagents never get the tools or the instructions.

Calling `exit_plan_mode` blocks until you answer: the plan pane opens beside the conversation and the approval card is drawn inside it. The card has a reply box and two answers: **Approve**, after which plan mode turns off and the model goes on to implement the plan in the same turn, or **Send feedback** with what should change, after which the model revises the plan and asks again — as many times as it takes. There is no refusal, and approving does not change the security level. A saved plan also appears as an **Implementation plan** row in the Tasks pane (More options → Tasks), and clicking it opens the same plan pane.

Some confirmations cannot be turned off by any level or by a hook: recursive deletes that touch the root, home or system paths, or whose target the classifier cannot resolve; writing global memory; MCP tools that declare `anthropic/requiresUserInteraction` or that `mcp.json` lists in `disabledAutoApproveTools`; and taking over a browser page you logged into yourself.

An approval card shows the tool, a one-line summary of its arguments (after any hook rewrite), the risk level and why the call was classified that way. **Always allow** remembers the decision for that tool in that conversation, capped at the risk level of the card you answered; shell tools, MCP tools and the eleven `preview_*` tools that act on the page never get a standing allowance. Cards raised by background tasks survive the end of the turn and reappear after a reload.

`fork` always raises a non-blocking request card, including at Full access, and never creates a child automatically. The child always starts with only its prompt; approved and declined decisions remain in the Tasks pane (More options → Tasks) but are never reported to the model.

## Run environments

Every workspace lives on a machine — this one, a WSL distribution, or an SSH machine you registered — and a shell is an execution backend of that machine. Mework probes each machine for the shells it can use there (this machine at startup, an SSH machine the first time it is reached each session, a WSL distribution on first use, and any machine from the **Probe shells again** button in its settings) and only looks for the combinations it supports: PowerShell and Git Bash on Windows; zsh, Bash and sh on macOS, Linux and WSL. The shell tools a conversation lists are the union of its machines' shells, and each tool can name only the workspaces whose machine has it. A WSL distribution or SSH machine also has an **agent shell**, chosen beside that button: the remote file tools, the language servers and the Git status read run their scripts through it. A new machine starts with the first of these it has, in Mework's fixed order for its OS: PowerShell before Bash on Windows; Bash, zsh, sh on Linux and WSL; zsh, Bash, sh on macOS. Environment variables belong to each workspace; the gear beside it edits them. Approvals are fingerprinted with the environment, so changing it invalidates a pending approval.

### Extra working directories

The folder-plus button at the end of that chip row adds a directory outside the workspace that this conversation may also work in. It opens the same native picker workspaces use, and that picker is the only thing that authorizes the path: a directory the host never handed back is refused on save, so nothing but your own selection can widen a conversation's reach. Each grant becomes its own chip with an × to give it back.

The model is told about them: the host opens its half of the system prompt with an **environment block** stating the primary working directory, whether it is a Git checkout (and whether it is an isolated worktree), every extra working directory, the platform, the OS version and today’s date. It is rendered fresh each turn from the host’s own records, and every line of it is a profile key you can translate or remove.

A granted directory is trusted exactly as the workspace is — reads, writes and shell paths inside it stop counting as escapes, so they no longer raise an approval card at Accept edits. It belongs to the one conversation you granted it to: presets, workspace snapshots and new conversations never inherit it, and a fork does because a fork holds the parent's permissions. A directory that has since been deleted or unplugged is simply dropped from the boundary rather than failing the call. The set is frozen while a turn is running.

## Subagents, workflows and tasks

Everything that keeps running is a **task** and shows in the Tasks pane with a stop button: subagents, workflow runs, background shell commands, terminals and the dev servers the conversation started.

- `agent_spawn` starts a subagent that runs in the background (up to 8 live at once). It gets the parent's tools minus the orchestration ones — no spawning, no `workflow`, no `ask_user`, no `fork`, and no view of the parent's task board — plus its own `task_wait`/`task_list` when it holds a task producer of its own. It sees a copy of the conversation history only when asked (`context: "conversation"`), and carries the child addendum from the prompt profile. A subagent is one-shot: the spawn hands it its whole task, it runs on its own, and nothing can message it or give it more work afterwards. `task_wait` blocks until the named tasks settle and returns their results; anything not collected is delivered at the next round boundary as a `<task-notification>`.
- **Roles** (agent definitions on the preset or conversation) let the model pick a subagent by name; you decide the model, tools and search backend behind that name. When roles exist, `agent_spawn` requires one unless **Allow role-less subagents** is on.
- `workflow` runs a JavaScript orchestration script — `agent()`, `parallel()`, `pipeline()`, `phase()`, `log()`, `args`, `budget` — in a private pool with a persisted journal. Runs survive restarts, can be resumed with `resume_run_id`, and can isolate steps in git worktrees. It is an ordinary tool, off by default.
- `ask_user` blocks on a question card above the composer; your answers come back as its result, in the same turn.

## Auto-compact and handoff {#auto-compact}

Long work fills the context window. The context meter in the composer shows how full it is, and its **Auto-compact** menu switches the feature (on by default) and sets the threshold, 20–97% of the model's context window (80% by default). Mework never summarizes a conversation behind the model's back. When the context crosses the threshold, the host asks the model between rounds to hand off — as a mid-conversation system message where the model and endpoint take one, else in the same `box` delivery that brings a background task's result — and offers it four tools: it writes handoff notes (`create_handoff_note`, `edit_handoff_note`) into a notebook of the conversation's own under the application data directory, where no file tool reaches, then calls `handoff`. That opens a new conversation as a fork of this one, starts it and ends this run. The new conversation starts with this conversation's system prompt (its first card, when that is a system card), the same settings and tools, and the notes' index: as the last section of its system prompt on a model that reads its tools ahead of its system prompt (Claude, OpenAI's GPT models, Kimi, GLM), and otherwise handed over right after its first message — as a mid-conversation system message where the model and endpoint take one, else in `box` — so it never sits ahead of the tools. Its first message, the prompt profile's `handoff.start_message`, asks it to read the notes and carry on. None of the history goes with it, and the original conversation is left as it was. `handoff` waits while background agents or workflows are still running, because their results come back to the conversation that started them.

A model without the **Mid-conversation tools** capability (see [the tool lock](tools.html#lock)) never gets the four tools part-way through a run, so auto-compact does not arm for it, and the menu says **Not for this model**.

Like every host text, the request to hand off (`handoff.armed_notice`, the whole message in either form) and the opening message can be reworded in a prompt profile; the `box` form is explained by the `box` tool's own description, which every conversation carries.

## Web search and fetch

`web_search` has two backends: **native** (the conversation's own model runs its provider-side search in an isolated request with no client tools) and a **catalog provider** (Tavily, Exa, SearXNG, Jina, Firecrawl, Bocha, Zhipu, …) that the host calls itself. `web_fetch` picks its backend separately, because upstreams disagree about how many web tools there are: Anthropic splits retrieval into its own server tool, while OpenAI keeps it inside its one search tool. So fetching may be **native** too — which grants a second tool on the first kind of provider and none on the second — or a catalog provider, or off, and a conversation with web access on but both backends off simply has no web tools. The menus list only providers switched on under Settings → **Search providers**, which is also where each one's key or host lives. Both tools are asynchronous: one approval covers every network action of the call, and results come back as the tool result marked `untrustedWebContent`. Once a conversation has used either tool, its backend is fixed for the rest of that conversation — a transcript holds results only the backend that produced them can be replayed against.

## The built-in browser

`Ctrl+B` toggles it. It is a real browser you drive — address bar, back/forward, reload, an element picker, an annotation layer, a dev-server picker and a resizable log drawer — and it is also the surface the model verifies its own work on.

The model's half starts from `.mework/launch.json` in the workspace:

```json
{
  "version": "0.0.1",
  "configurations": [
    { "name": "dev", "runtimeExecutable": "npm", "runtimeArgs": ["run", "dev"], "port": 5173 }
  ]
}
```

`preview_start` looks a server up by name, spawns it, waits for the port to answer and points the conversation's page at `http://localhost:<port>`; an entry with a `url` and no command attaches to a server something else already started. A localhost `url` has to be a bare origin on the entry's port — a repository cannot make the pane open `http://localhost:9090/admin/wipe` the moment you open the project. `preview_stop` and `preview_list` manage the processes, and `preview_logs` returns the server's own stdout/stderr from a bounded ring buffer (1000 chunks), filtered by level or by a search string. Up to five servers run per worktree.

The other eleven tools act on the page: `preview_snapshot` (accessibility tree), `preview_inspect` (computed styles for one selector), `preview_screenshot`, `preview_console_logs`, `preview_network`, `preview_click`, `preview_fill`, `preview_eval`, `preview_resize`, `preview_dialog` and `preview_upload_image`. There is deliberately no navigate tool and no tab management: one conversation gets one page, and it comes from the server the model started (`preview_eval` can still move it with `window.location`). Input goes through trusted CDP with a visible pointer overlay; nothing is injected into the page, and remote content can never reach the app's IPC surface.

Five of the fifteen read only the host's own state and never raise an approval card: `preview_list`, `preview_logs`, `preview_snapshot`, `preview_inspect` and `preview_resize`. The other ten ask below Full access.

When a conversation has several workspaces the pane is organised in pages: each page belongs to one workspace, its start page lists that workspace's `.mework/launch.json`, and the servers it runs run on that workspace's machine; the number on a tab is the one the composer's workspace chip shows. The top bar's preview button lists the workspaces — picking one opens a page for it — with a last row that shows or hides the pane; with no page yet, that opens the start page of the workspace the chip has selected. The pane's **+** lists the same workspaces.

For a workspace on an SSH machine, the dev server is started and kept by Mework's agent on that machine: the configuration is read, the port probed and readiness waited out there. A dropped link leaves the server running, its output kept on the machine and delivered in order once the link is back. The page is rendered here, but every connection it opens is opened from that machine through the agent — `localhost` in the page, and every name it resolves, are the machine's, so the API on the next port answers too. While the link is down the pane says it is reconnecting rather than leaving a timeout to read as a broken page.

Every page is a single-use, isolated WebView2 profile: no shared cookies, nothing imported from your daily browser, destroyed on close. If you log into a site in a page yourself, the model has to ask before it can act on it — a confirmation no security level and no hook can turn off.

## Long-term memory

Two tiers of plain Markdown, each with the same layout:

```text
~/.mework/                       <workspace>/.mework/
  MEWORK.md                        MEWORK.md        standing instructions (you write these)
  memory/MEMORY.md                 memory/MEMORY.md index (the host maintains it)
  memory/<topic>.md                memory/<topic>.md memory documents (the model writes these)
```

With a tier's switch on, its `MEWORK.md` and `MEMORY.md` are pasted into the request and its three tools become available; document bodies are fetched by name when needed. Global memory writes always ask for confirmation. Memory is low-priority context for the model, not policy: rules that must always hold belong in hooks or the security level.

Project instruction files named `MEWORK.md` — in the workspace, in its `.mework/`, in ancestor directories up to the project boundary, and in `~/.mework/` — together with `.mework/rules/*.md` and a machine-managed policy file, are discovered at startup and injected as an untrusted file-context block, independently of the memory switches.

## Images and files {#images}

Attach files to a message with **+ → Upload files**, by pasting, or by dropping them onto the composer — or onto a message you are editing on the timeline, which has its own **+**. Images, PDFs and text files (source code, Markdown, CSV, JSON, logs and the like) are accepted; while you drag, the drop target says what it will take, and a folder or an unsupported format shows as unavailable before you let go. Anything turned away is listed with the reason. Click an attachment to preview it: a PDF is laid out, Markdown, HTML, CSV and notebooks are rendered, and other text is shown with line numbers.

Each image gets an `[Image #N]` placeholder in your message that the model can refer to (the selected model must accept images). Tool results that produce images (`read` on an image file, `preview_screenshot`) get numbers too, and `preview_upload_image` can send one of them to a file input on the page. A file reaches the model as its text, placed ahead of your message when the request is sent — a PDF as the text of its pages, so a scan without a text layer is refused. A text file can be up to 512 KB and a PDF up to 10 MB; a message can carry up to 20 files, holding about 400k tokens of text in all. Pasting a long run of text (5,000 characters or 100 lines) turns it into a `pasted-text.md` attachment instead of filling the box. Attachments are stored content-addressed under the app-data directory.

## Git and terminals

The **Git review** panel shows the workspace's changes as one diff — all changes, staged only, or unstaged only — readable file by file or side by side, with stage, unstage and a confirmed discard on each file, and it carries an in-progress merge, rebase, cherry-pick, revert or bisect to its next step. The branch is switched from the chip above the composer, where a conversation can also be put in a dedicated **worktree**. The **terminal** panel opens shells you drive yourself, in tabs — PowerShell on Windows, `$SHELL` elsewhere; the model sees each one only as a task it can wait on.

## Keyboard, appearance, dependencies

Settings → **Keyboard shortcuts** lists the application's commands and lets you rebind them (zoom keys are fixed). **Appearance** covers theme (light, dark, following the system, or a picture of your own behind frosted panes), accent, application language, zoom, fonts, message layout and rendering, the send and newline keys, and custom CSS. Its **Local model** card turns on three optional helpers that run a small model on your own computer, never a provider: naming a conversation from its first message, a one-line description of each shell command shown as that command's card title (for example "zsh: Run the API tests"), and a few words on why a failed tool call or command failed, after what failed on its card's title (for example "Bash command failed: pnpm is not installed"; until the model answers, and with this helper off, the title quotes the error's own first line). By default these work for the conversation itself; **Also for subagents** extends them to subagents and workflow steps, and names each workflow step for its row in the task panel. Their requests wait behind the conversation's own, and when too many are waiting a subagent's give way first. Turning either on asks which build of Qwen3.5-0.8B (the official instruct model) to download, from Hugging Face or, if that is unreachable, ModelScope; every file is checked against its SHA-256. On a Mac the builds come converted ahead of time and are offered by chip: on a Mac with a Neural Engine, one that runs there through Core ML (about 1.4 GB, macOS 15 or later; the first load compiles it, about two minutes), and on any Mac with Apple silicon, one that runs on the GPU with MLX (about 1.5 GB, macOS 14 or later). On Windows and Linux the build (about 1.6 GB) is a GGUF of the model plus llama.cpp's own release build to run it, which uses the GPU through Vulkan (any vendor) where there is one and the CPU otherwise. **Manage…** shows each build's download and device, lets you keep several builds and switch the one in use, and lets you edit the three system prompts; each prompt is kept as the model's KV cache, and the line under it gives its token count and the cache's size. **Dependencies** probes `git`, `node`, `python`, `rg`, `fd`, `gh`, `uv`, `bun` on your `PATH`, and you can add your own command with its version arguments — detection only, no installs.

## Data and reset

The settings document carries a schema version: one written by a newer build is refused rather than downgraded, and a document that fails to load is left untouched while a `document.corrupt-<timestamp>.json` copy is written beside it. To start over during development:

```bash
npm run reset:data           # wipe app data (add --keys to also remove stored API keys)
```

Uninstalling the app does not delete `%APPDATA%\com.mework.app`, `%LOCALAPPDATA%\com.mework.app`, the Credential Manager entries or `~/.mework`.
