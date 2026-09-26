# MCP servers

Mework is an MCP **client**. Declare a server in an `mcp.json` file, select it in the conversations that should see it, and its tools are discovered at the start of every turn and offered to the model next to the built-in tools. Two transports are supported:

- **stdio** — Mework launches a process and speaks JSON-RPC over its stdin/stdout.
- **Streamable HTTP** — Mework connects to a URL. The older HTTP+SSE transport is not supported.

## Where servers are declared

MCP servers are **not** registered in the app. They are declared in a file, the same way most MCP clients do it:

```text
~/.mework/mcp.json                 user scope — every workspace
<workspace>/.mework/mcp.json       workspace scope — that workspace only
```

A fresh installation writes four entries into the user file — `builtin_sequential_thinking`, `builtin_context7`, `builtin_fetch` and `builtin_time` — and the built-in **mework** preset selects them. Each launches through `npx` or `uvx` and needs no credential.

The shape is Claude Code's `.mcp.json`: a top-level `mcpServers` object keyed by server name.

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "C:/projects"],
      "env": { "LOG_LEVEL": "info" }
    },
    "docs": {
      "type": "http",
      "url": "http://127.0.0.1:3000/mcp",
      "headers": { "Authorization": "Bearer ${DOCS_TOKEN}" }
    }
  }
}
```

### Accepted keys

| Key | Meaning |
|---|---|
| `type` | `stdio`, or `http` / `streamable-http` / `streamable_http` for Streamable HTTP. With a `command` and no `type`, the entry is stdio. |
| `command`, `args`, `env` | stdio: the process to launch, its arguments, and environment variables for it. `command` is executed directly, never through a shell. |
| `cwd` | stdio: working directory for the process. It must be an absolute path to a directory that exists. |
| `envPassthrough` | stdio: names of Mework's own environment variables to pass through to the process. The child starts from a cleared environment plus a small allowlist that lets it find programs and caches (`PATH`, `PATHEXT`, `SystemRoot`, `WINDIR`, `COMSPEC`, `TEMP`, `TMP`, `TMPDIR`, `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `PROGRAMDATA`, `ProgramFiles`, `ProgramFiles(x86)`, `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`), so anything else the app holds reaches the server only through this key or `env`. `env` wins over both. A name starting with `MEWORK_`, `ANTHROPIC_`, `OPENAI_`, `CLAUDE_`, `CODEX_`, `AWS_`, `AZURE_`, `GOOGLE_`, `GEMINI_` or `DEEPSEEK_` is refused — the app's own provider credentials are not passable — and the entry reads unavailable. |
| `url`, `headers` | http: the endpoint and extra request headers (typically `Authorization`). Public hosts must use `https`; loopback and private-network addresses may use `http`. |
| `timeout` | Per-request timeout in **milliseconds** (Claude Code's key). A value below 1000 is ignored and the host default applies; anything else is rounded up to whole seconds. |
| `timeoutSeconds` | The same timeout in whole seconds (Mework's key). It wins over `timeout`, and is clamped to 5 minutes; `0` uses the host default (45 s). |
| `longRunning` | Marks a server whose calls legitimately take long: without an explicit timeout its calls get the 5-minute ceiling instead of the 45-second default. |
| `description` | Shown in the catalog and, when selected, listed in the system prompt section `system.mcp_section`; truncated at 240 characters. An entry without one is listed as `system.mcp_server_default_description`. |
| `registryUrl` | A package-registry mirror. Applied as `npm_config_registry` for `npx`, `npm`, `bun`, `bunx`, `pnpm` and `yarn` commands, and as `UV_INDEX_URL` + `PIP_INDEX_URL` for `uv`, `uvx`, `pip`, `pipx`, `python` and `python3`; only http(s) URLs; a variable you set yourself in `env` wins. |
| `disabledTools` | Tool names not offered to the model. This is an exclusion list: a server that gains a tool makes it available until you add its name here. |
| `disabledAutoApproveTools` | Tool names whose auto-approve is off: every call asks, at every level, like a `requiresUserInteraction` declaration. |
| `${VAR}` / `${VAR:-default}` | Expanded in `command`, `args`, `env` values, `url` and `headers` values. |

A server name may contain only letters, numbers, `-` and `_`, and may not be `__proto__`, `constructor` or `prototype`. A `type` of `sse`, `ws` or `sdk`, an entry using `headersHelper` or `oauth`, and a `${VAR}` reference to a variable that is unset with no `:-default` all leave the row in the catalog — marked unavailable, with the reason as its description — so a file copied from another client explains itself instead of silently shrinking. A top-level `servers` key or a JSON array yields nothing (with a note on the host's stderr); unknown keys are ignored. Every parsed entry is validated the way a run would validate it, so an entry Mework could not dial reads as unavailable with the message.

Only **tools** are offered to the model. The prompts and resources a server exposes are read by the connection test and go no further.

Each available row has a **Test connection** button: it dials the server once, handshakes, and lists its tools, prompts and resources. The row's badge then reports how many tools it found, with the name and version the server gave in the row's tooltip — or **Connection failed**, with the error and the last lines the server wrote to its stderr. An unavailable row is never dialed; its badge reads **Unavailable** and the reason is in the tooltip. The row's delete button removes the key from the `mcp.json` it was declared in, leaving every other entry and every other top-level key as you wrote them; a server this conversation has already handed to the model is locked and carries neither button.

## Selecting servers for a conversation

As with skills, discovery makes a server available and the conversation drawer's **MCP** page decides which ones this conversation uses (presets template the same field). A conversation can select from the global `~/.mework/mcp.json` plus **its own workspace's** `.mework/mcp.json`; another workspace's servers are not offered, and a preset may keep any id. The selected servers are listed in the system prompt so the model knows they exist; whether their tools are actually callable is decided by the discovery at the start of the turn.

A selected id the scan no longer finds stays selected and shows as a **dangling** row; it is **skipped** at run time. An entry the scan does find but cannot dial is shown unavailable with the reason, and a run that selects it **fails** until you fix it or uncheck it. The page toolbar has a **Rescan** button and two **open folder** buttons — the global `~/.mework` and, for a conversation in a directory workspace, that workspace's `.mework`. Each reveals `mcp.json` when the file is there, and otherwise creates the directory and opens that.

## How MCP tools reach the model {#how-mcp-tools-reach-the-model}

At the beginning of each turn the host connects to every selected, available server (sessions are per conversation, pooled, and closed after 30 minutes idle), calls `tools/list`, and gives every tool it finds a collision-resistant name:

```text
mcp__<server-slug>_<server-digest>__<tool-slug>__<tool-digest>
```

Under that name the model gets the server's own title, description and input schema; **Tool discovery** below decides whether that goes out with every request or is fetched when the model asks for it. Tool names in `disabledTools` are dropped at discovery, so the model never sees them. If a server fails to connect, its tools are absent for that turn and the failure is logged; the turn itself continues. Subagents inherit the parent's discovered bindings, narrowed by their role's own tool list.

A call is translated back to `tools/call` with the tool's original name; the `content`, `structuredContent` and `isError` of the reply are returned to the model as JSON (server `_meta` is stripped). Replies over 64 KiB are refused rather than truncated.

### Tool discovery

The **Tool discovery** switch (under the list, always shown) chooses how the discovered tools are handed over:

- **Off, the switch reading *All declared*.** Every discovered tool's full schema goes out with every request, exactly like a built-in tool's.
- **On, *On demand* — fetched with the [`tool_search` tool](tools/tool_search.html).** No MCP schema is declared. The context carries a `<deferred-tools>` block instead, listing the withheld names grouped by the server that declared them (`tool_search.announcement` and `tool_search.announcement_row` in the [prompt profile](prompt-profiles.html)). `tool_search` takes a `query`: `select:<name>[,<name>…]` returns those exact definitions, anything else is a keyword search over the withheld names, their servers and their descriptions (a `+term` every result must contain, the rest ranking) capped by `max_results`, default 5. A tool whose definition has come back is declared normally from the next step on and callable like any other; calling one whose schema has not been fetched is rejected with the `select:` call that fixes it (`tool_search.not_loaded`).

A new conversation takes the switch from the preset it starts from; the built-in **mework** preset starts it on. Once any server has been dialed the switch is frozen, the same way skill delivery is: the transcript already carries the tools one way. The announcement is fixed for the whole run, so a name stays listed after its schema arrives. A subagent inherits the mode and withholds every inherited tool again, since it starts from an empty history and never read the parent's `tool_search` results.

`tool_search` is derived, not in the tool picker: it appears exactly when the switch is on and the run withheld at least one tool.

## Approvals

MCP tools are treated as external side effects:

- At `request_approval`, `allow_edits` and `plan`, every call asks for confirmation unless a `PreToolUse` or `PermissionRequest` hook allows it. At `full_access` calls run without asking.
- A tool that declares `_meta["anthropic/requiresUserInteraction"] = true` asks **on every call**, at every level, and a hook cannot pre-approve it. Its description to the model is prefixed with `mcp.mandatory_description_prefix`, and its label carries a note saying the same. A malformed value fails closed to "ask".
- A tool named in `disabledAutoApproveTools` behaves the same way: the same prefix, and it asks at every level.

## Verifying a server end to end

1. Write the entry in `~/.mework/mcp.json` or the workspace's `.mework/mcp.json`; the row appears in the drawer's **MCP** page (press **Rescan** if the page was open). Click **Test connection** on the row: the badge should report how many tools it found, or the failure with the server's stderr in the tooltip.
2. Select it in a conversation, send a message that needs one of its tools, and look for a tool card named after the server in the timeline; approve the call when the card appears.
3. If nothing happens: check the system prompt section is present (the server must be selected), then re-read the **Test connection** result or run the same command in a terminal (a stdio server that exits at startup produces no tools), then the model — providers that do not support tool calling never see MCP tools.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The server is not in the list | Its entry is not in `~/.mework/mcp.json` or this workspace's `.mework/mcp.json`, or the file is not an `{"mcpServers": {…}}` object (a top-level `servers` key or an array is not read), or its name has characters outside letters, numbers, `-`, `_`. |
| The row says "Missing environment variables: …" | A `${VAR}` reference in `command`, `args`, `env`, `url` or `headers` has no value and no `:-default`. Set the variable, add a default, or write the value literally. |
| The row is unavailable with a transport reason | `type` is `sse`, `ws` or `sdk`, a `url` is given without a `type`, or the entry uses `headersHelper` or `oauth`. Use `"type": "http"`, put headers in `headers`, or send a token there. |
| Test connection fails with a spawn error | `command` is not on the app's `PATH` or the arguments are wrong. Try the same command in a terminal. On Windows the executable is resolved through `PATH` and `PATHEXT` (so `npx` finds `npx.cmd`), but only `.COM`, `.EXE`, `.BAT` and `.CMD` are honoured, and the command line is not passed through a shell — give an executable name or full path, not a pipeline. |
| Tools appear after a test but the model never calls them | The server is not selected in the conversation, or the provider/model has no tool calling. |
| The model says a tool's schema is not loaded | **Tool discovery** is on, so the schema is fetched, not declared. The rejection names the `tool_search` call that loads it; the `<deferred-tools>` block lists every name that is waiting. |
| Every call asks even at Full access | The tool declares `requiresUserInteraction`, or it is named in `disabledAutoApproveTools`. |
| Two servers expose the same tool | Names carry a per-server digest, so both are callable; the description tells them apart. |
| An HTTP server refuses `http://` | Only loopback/private addresses may use plain HTTP; public endpoints need `https`. |
| A change to the file is not visible | The list on screen is a snapshot. Press **Rescan** on the page; startup, deleting an entry and adding a workspace scan too. A run always reads the files fresh, so a saved change takes effect on the next turn whether or not the list has caught up. |
