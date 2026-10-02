# Built-in tools

Mework ships {{TOOL_COUNT}} built-in tools. Each has its own page with what it does for you, when it asks for approval, its parameters as the model sees them, and the description the built-in prompt profile gives the model. The tool names are the ones the model calls and the ones a [tool-description file](prompt-profiles.html) refers to.

The tools below are grouped the way the tool picker groups them. Every switchable tool is a row of its own there, and the book icon at the start of the row leads to its page here.

## How a tool becomes available {#available}

{{SWITCHABLE_TOOL_COUNT}} of the tools are switches in the **Enabled tools** list of a conversation or preset (the sliders icon next to the composer, or Settings → Conversation presets). The other {{DERIVED_TOOL_COUNT}} are **derived** from another setting or tool and never appear in that list:

| Derived tools | Follow |
|---|---|
| `read_global_memory`, `create_global_memory`, `edit_global_memory` | the **global memory** switch |
| `read_project_memory`, `create_project_memory`, `edit_project_memory` | the **project memory** switch |
| `web_search`, `web_fetch` | the conversation's single **Enable web search** switch; which of the two a run gets depends on the resolved backend |
| `task_wait`, `task_list` | any task-producing tool being enabled (`agent_spawn`, `workflow`, `bash`, `zsh`, `sh`, `powershell`, `preview_start`) |
| `box` | every run: it carries every message the host appends to a conversation |
| `skill` | the **Load skills on demand** switch, when at least one skill is selected |
| `tool_search` | the **Tool discovery** switch (MCP), when a run is holding MCP tool schemas back |
| `plan`, `exit_plan_mode` | the conversation's **Plan mode** switch |
| `preview_start`, `preview_stop`, `preview_list` | any other `preview_*` tool being enabled |

### The tool lock {#lock}

The tool surface a conversation's last request went out with — its tools, MCP servers, memory tiers, web access, Plan mode, skills and the two delivery switches — is its **tool lock**, and it belongs to the model that sent that request. While that model is the selected one, the settings pane draws the lock on the rows themselves, each with a lock at its right:

- **Orange** — the model's prompt cache is still warm, and changing the row would rewrite the part of the prompt it holds: turning off a tool, Plan mode, web access or a skill the last request carried, or moving an MCP server, a memory tier, **Load skills on demand** or **Tool discovery** either way. An orange row still moves. The first time you move one in a conversation, Mework says the cache will be lost and asks before making the change, with a **Don't show this again** box that silences the warning everywhere. Moving the row back before the next request keeps the cache, and it turns orange again. Turning a tool on is never orange: the tool is appended (below) and nothing cached is lost. Each model's profile under **Settings → Providers** has a **Cache lifetime** in minutes (30 by default); once that long has passed since the last request, the orange goes.
- **Gray** — the model cannot take a tool mid-conversation (see below), so its whole tool surface is fixed at the first request: no tool joins and none leaves, cache or no cache. Skills are not tools and stay free.

Selecting another model lifts both, since its cache holds none of this; selecting the model again puts back what its lock held. The search and fetch backends a conversation has used are a different matter and stay fixed for good, whatever the model: a transcript's search results can only be replayed against the backend that produced them.

A tool that joins mid-conversation — one you tick in the settings, the handoff tools **Auto-compact** arms, an MCP tool `tool_search` fetched — is **appended** rather than written back into the tool list every request declares: that list heads the prompt, and rewriting it invalidates the whole prompt cache. The host records in the timeline where the tool joined, and the protocol's own append interface hands the tool to the model at that point, with no text announcing it. Anthropic Messages uses a mid-conversation `tool_addition` (the tool itself declared with `defer_loading`); OpenAI Responses, Azure and Codex use an `additional_tools` input item. **Claude Agent** lets Claude Code append the tool itself, as its own `tool_addition` on the models it supports, with one line of its own text saying the tool is available. Every other protocol has no append interface.

Whether a model takes a tool this way is one of its capabilities, beside vision: **Mid-conversation tools**, in the model's properties under **Settings → Providers**. Mework ticks it itself for the models it knows — when a model is fetched, and as you type the ID of one you add: at Anthropic's own API, Fable 5, Mythos 5, Opus 4.8 and later, and Sonnet 5.5; through **Claude Agent**, the same list without Sonnet 5.5; at OpenAI's or Azure's own endpoints and through Codex, every model. A model it does not know — one behind a relay above all, which may or may not pass the interface on — starts unticked, and ticking it is yours: the addition then goes to that endpoint as it would to the vendor's. On a protocol without an append interface the capability does nothing. A model without it has its tool surface fixed at the first request ([gray](#lock)); **Auto-compact** does not arm and **Tool discovery** is not offered. An endpoint that refuses an addition all the same gets the tool in the declared list.

## Approvals

Every call is classified on the host before it runs — as a read, a write, or an unbounded action, inside or outside the workspace — and the conversation's security level decides which classes ask you first. Tools marked **Reviewed** in the picker are the ones whose calls are writes or unbounded actions. Some confirmations no level turns off: recursive deletes that reach the workspace root, your home or a system path; writing global memory; MCP tools that declare they need the user; and acting on a preview page you have logged into yourself. The [security level table](working.html#tools-and-approvals) has the full matrix; each tool page states what applies to it.

## Files and search {#group-filesystem}

The picker's **Files and search** group: the six file tools, and `lsp`.

### File tools {#files}

The six tools that list, search, read and change the workspace's files. `write` and `edit` are marked **Reviewed**. All six are bounded to the workspace and the roots granted to the conversation, and the write guards under [Tools and approvals](working.html#tools-and-approvals) apply to every one of them.

{{TOOL_TABLE:ls,find,grep,read,write,edit}}

### `lsp` — code navigation {#filesystem-standalone}

`lsp` asks a language server about symbols rather than text — where a name is defined, who references it, who calls a function. [Code navigation](lsp.html) covers the servers it runs and how they are configured.

{{TOOL_TABLE:lsp}}

## Shell {#group-shell}

### Shell tools {#shell}

One tool per shell backend — `bash`, `zsh`, `sh` and `powershell`. The picker lists only the shells found on the conversation's machines: a Windows machine can offer `powershell` and `bash`, a macOS, Linux or WSL machine `zsh`, `bash` and `sh`. Every shell tool is marked **Reviewed**, and every one produces tasks, so the task tools come with it.

{{TOOL_TABLE:bash,zsh,sh,powershell}}

## Preview {#group-web}

The picker's **Preview** group holds the preview tools. The two web tools sit here too, though they never appear in the picker.

### Preview tools {#preview}

The `preview_*` tools run the project's dev server and act on the page it serves in the built-in browser. `preview_start`, `preview_stop` and `preview_list` have no row in the picker: they are on whenever any other preview tool is. `preview_screenshot` and `preview_upload_image` are only offered to models that can see images. Servers start from the configurations in `.mework/launch.json` — see [The built-in browser](working.html#the-built-in-browser).

#### Dev servers {#preview-servers}

{{TOOL_TABLE:preview_start,preview_stop,preview_list,preview_logs}}

#### Read the page {#preview-observe}

{{TOOL_TABLE:preview_console_logs,preview_screenshot,preview_snapshot,preview_inspect,preview_network}}

#### Act on the page {#preview-act}

{{TOOL_TABLE:preview_click,preview_fill,preview_eval,preview_resize,preview_upload_image,preview_dialog}}

### Web access {#web-access}

Both follow the conversation's single **Enable web search** switch; which of the two a run gets depends on the resolved backend.

{{TOOL_TABLE:web_search,web_fetch}}

## Agent orchestration {#group-orchestration}

{{TOOL_TABLE:agent_spawn,task_wait,task_list,box,workflow,fork,ask_user,skill,tool_search,plan,exit_plan_mode}}

## Long-term memory {#group-memory}

{{TOOL_TABLE:read_global_memory,create_global_memory,edit_global_memory,read_project_memory,create_project_memory,edit_project_memory}}

## Handoff {#group-handoff}

None of these is a setting. With **Auto-compact** on (in the context meter's menu), a conversation whose context crosses the threshold is asked by the host to hand off — as a mid-conversation system message where the model and endpoint take one, else in the same `box` delivery a background task's result takes — and gains these four tools. The model writes handoff notes, then calls `handoff`, which opens a continuation (a new conversation, not a fork) and stops this one. The new conversation starts with this conversation's system prompt, the same tools and those notes, and nothing of its history. A conversation that inherited notes can read them from its first request.

{{TOOL_TABLE:read_handoff_note,create_handoff_note,edit_handoff_note,handoff}}
