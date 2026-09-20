# Built-in tools

Mework ships {{TOOL_COUNT}} built-in tools. Each has its own page with what it does for you, when it asks for approval, its parameters as the model sees them, and the description the built-in prompt profile gives the model. The tool names are the ones the model calls and the ones a [tool-description file](prompt-profiles.html) refers to.

## How a tool becomes available

{{SWITCHABLE_TOOL_COUNT}} of the tools are switches in the **Enabled tools** list of a conversation or preset (the sliders icon next to the composer, or Settings → Conversation presets). The other {{DERIVED_TOOL_COUNT}} are **derived** by the host from another setting and never appear in that list:

| Derived tools | Follow |
|---|---|
| `read_global_memory`, `create_global_memory`, `edit_global_memory` | the **global memory** switch |
| `read_project_memory`, `create_project_memory`, `edit_project_memory` | the **project memory** switch |
| `web_search`, `web_fetch` | the conversation's single **Enable web search** switch; which of the two a run gets depends on the resolved backend |
| `task_wait`, `task_list`, `box` | any task-producing tool being enabled (`agent_spawn`, `workflow`, `bash`, `powershell`, `preview_start`) |
| `skill` | the **Load skills on demand** switch, when at least one skill is selected |
| `tool_search` | the **Tool discovery** switch (MCP), when a run is holding MCP tool schemas back |
| `plan`, `exit_plan_mode` | the **security level** being Plan mode |

Tool exposure is one-way within a conversation. Once a run has shown the model a tool — or an MCP server, a memory tier, on-demand skill loading, web access — that surface stays on for the rest of the conversation and the settings pane grays it out: a transcript that already calls a tool cannot be replayed to a model that no longer has it. Later rounds can widen the surface, never narrow it.

## Approvals

Every call is classified on the host before it runs — as a read, a write, or an unbounded action, inside or outside the workspace — and the conversation's security level decides which classes ask you first. Tools marked **Reviewed** in the picker are the ones whose calls are writes or unbounded actions. Some confirmations no level turns off: recursive deletes that reach the workspace root, your home or a system path; writing global memory; MCP tools that declare they need the user; and acting on a preview page you have logged into yourself. The [security level table](working.html#tools-and-approvals) has the full matrix; each tool page states what applies to it.

## All tools

{{TOOL_INDEX}}
