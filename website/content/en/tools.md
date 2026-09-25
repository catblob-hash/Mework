# Built-in tools

Mework ships {{TOOL_COUNT}} built-in tools. Each has its own page with what it does for you, when it asks for approval, its parameters as the model sees them, and the description the built-in prompt profile gives the model. The tool names are the ones the model calls and the ones a [tool-description file](prompt-profiles.html) refers to.

The tools below are grouped the way the tool picker groups them. Three sets of tools are one capability split into many calls — the files, the shells and the preview — and the picker shows each set as a single row, `files`, `shell` or `preview`, that opens a window of its own; each of the three has its own section here, and the book icon beside that row in the picker leads to it.

## How a tool becomes available

{{SWITCHABLE_TOOL_COUNT}} of the tools are switches in the **Enabled tools** list of a conversation or preset (the sliders icon next to the composer, or Settings → Conversation presets). The other {{DERIVED_TOOL_COUNT}} are **derived** by the host from another setting and never appear in that list:

| Derived tools | Follow |
|---|---|
| `read_global_memory`, `create_global_memory`, `edit_global_memory` | the **global memory** switch |
| `read_project_memory`, `create_project_memory`, `edit_project_memory` | the **project memory** switch |
| `web_search`, `web_fetch` | the conversation's single **Enable web search** switch; which of the two a run gets depends on the resolved backend |
| `task_wait`, `task_list`, `box` | any task-producing tool being enabled (`agent_spawn`, `workflow`, `bash`, `zsh`, `sh`, `powershell`, `preview_start`) |
| `skill` | the **Load skills on demand** switch, when at least one skill is selected |
| `tool_search` | the **Tool discovery** switch (MCP), when a run is holding MCP tool schemas back |
| `plan`, `exit_plan_mode` | the **security level** being Plan mode |

Tool exposure is one-way within a conversation. Once a run has shown the model a tool — or an MCP server, a memory tier, on-demand skill loading, web access — that surface stays on for the rest of the conversation and the settings pane grays it out: a transcript that already calls a tool cannot be replayed to a model that no longer has it. Later rounds can widen the surface, never narrow it.

## Approvals

Every call is classified on the host before it runs — as a read, a write, or an unbounded action, inside or outside the workspace — and the conversation's security level decides which classes ask you first. Tools marked **Reviewed** in the picker are the ones whose calls are writes or unbounded actions. Some confirmations no level turns off: recursive deletes that reach the workspace root, your home or a system path; writing global memory; MCP tools that declare they need the user; and acting on a preview page you have logged into yourself. The [security level table](working.html#tools-and-approvals) has the full matrix; each tool page states what applies to it.

## Files and search {#group-filesystem}

The picker's **Files and search** group: the `files` row, and `lsp`, which keeps a row of its own.

### `files` — file tools {#files}

The six tools that list, search, read and change the workspace's files are one row in the picker, `files`. Its count says how many of the six are on; clicking it opens the **File tools** window, where each has its own switch, and the **+** on the group heading switches all six on together with the rest of the group. `write` and `edit` are marked **Reviewed**. All six are bounded to the workspace and the roots granted to the conversation, and the write guards under [Tools and approvals](working.html#tools-and-approvals) apply to every one of them.

{{TOOL_TABLE:ls,find,grep,read,write,edit}}

### `lsp` — code navigation {#filesystem-standalone}

`lsp` asks a language server about symbols rather than text — where a name is defined, who references it, who calls a function. [Code navigation](lsp.html) covers the servers it runs and how they are configured.

{{TOOL_TABLE:lsp}}

## Shell {#group-shell}

### `shell` — shell tools {#shell}

One tool per shell backend — `bash`, `zsh`, `sh` and `powershell` — behind one row in the picker, `shell`. Its window lists only the shells found on the conversation's machines: a Windows machine can offer `powershell` and `bash`, a macOS, Linux or WSL machine `zsh`, `bash` and `sh`. The **+** on the group heading switches the shells on. Every shell tool is marked **Reviewed**, and every one produces tasks, so the task tools come with it.

{{TOOL_TABLE:bash,zsh,sh,powershell}}

## Control {#group-web}

The picker's **Control** group holds the `preview` row. The two web tools sit here too, though they never appear in the picker.

### `preview` — preview tools {#preview}

The `preview_*` tools run the project's dev server and act on the page it serves in the built-in browser; the picker shows them as one row, `preview`. Its window has three pages, one table each below. `preview_screenshot` and `preview_upload_image` are only offered to models that can see images. Servers start from the configurations in `.mework/launch.json` — see [The built-in browser](working.html#the-built-in-browser).

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

{{TOOL_TABLE:agent_spawn,send_message,followup_task,task_wait,task_list,box,workflow,fork,todo,ask_user,skill,tool_search,plan,exit_plan_mode}}

## Long-term memory {#group-memory}

{{TOOL_TABLE:read_global_memory,create_global_memory,edit_global_memory,read_project_memory,create_project_memory,edit_project_memory}}
