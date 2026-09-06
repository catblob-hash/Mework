# MCP servers

Mework is an MCP **client**. Register a server once under Settings → **MCP**, select it in the conversations that should see it, and its tools are discovered at the start of every turn and offered to the model next to the built-in tools. Two transports are supported, because those are the two the protocol specifies today:

- **stdio** — Mework launches a process and speaks JSON-RPC over its stdin/stdout.
- **Streamable HTTP** — Mework connects to a URL. The older HTTP+SSE transport is not supported.

Prompts and resources a server exposes are shown on its detail page for inspection; only **tools** are offered to the model.

## Registering a server

Settings → **MCP → Add ▾**:

- **Add server** opens an empty detail page.
- **Import from JSON** accepts the `mcpServers` document most MCP clients share, pasted into a dialog:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "C:/projects"],
      "env": { "LOG_LEVEL": "info" }
    },
    "docs": {
      "type": "streamable_http",
      "url": "http://127.0.0.1:3000/mcp",
      "headers": { "Authorization": "Bearer …" }
    }
  }
}
```

An entry with `url` (or `type: "streamable_http"` / `"http"`) becomes an HTTP server, anything else stdio. `description`, `timeoutSeconds`, `longRunning`, `disabledTools` and `disabledAutoApproveTools` are read when present. Duplicate names inside the document are reported instead of silently merged.

The detail page has these fields (the settings document stores them under `assets.mcpServers`):

| Field | Transport | Meaning |
|---|---|---|
| Name, Description | both | Shown in the catalog and, when selected, listed in the system prompt section `system.mcp_section` (the description defaults to `system.mcp_server_default_description`). |
| Enabled | both | Global switch. A disabled server is still listed in conversations as *selected, currently inactive*. |
| Command, Arguments, Environment | stdio | The process to launch. `command` is executed as-is; `env` is added to the process environment. |
| Registry URL | stdio | A package-registry mirror. Applied as `npm_config_registry` for `npx`/`bunx` commands and as `UV_INDEX_URL` + `PIP_INDEX_URL` for `uv`/`uvx`; only http(s) URLs; a variable you set yourself in `env` wins. |
| URL, Headers | HTTP | The endpoint and extra request headers (typically `Authorization`). Public hosts must use `https`; loopback and private-network addresses may use `http`. |
| Timeout (seconds) | both | Per-request timeout for this server's tool calls, up to 5 minutes; `0` uses the host default (45 s). |
| Long running | both | Marks a server whose calls legitimately take long: without an explicit timeout its calls get the 5-minute ceiling instead of the 45-second default. |
| Tools tab | both | After a successful test, each discovered tool has an **available** switch (persisted as `disabledTools`) and an **auto-approve** switch (persisted as `disabledAutoApproveTools`). |

Changes save immediately; there is no Save button. **Test connection** dials the server once, lists its tools, prompts and resources, records its name and version, and — for stdio — shows the process's stderr in the **Logs** tab, which is where startup failures explain themselves.

## Selecting servers for a conversation

As with skills, registration makes a server available and the conversation drawer's **MCP servers** section decides which ones this conversation uses (presets template the same field). A server must be both enabled and selected. The selected servers are listed in the system prompt so the model knows they exist; whether their tools are actually callable is decided by the discovery at the start of the turn.

## How MCP tools reach the model

At the beginning of each turn the host connects to every selected, enabled server (sessions are per conversation, pooled, and closed after 30 minutes idle), calls `tools/list`, and adds each tool to the model's tool list under a collision-resistant name:

```text
mcp__<server-slug>_<server-digest>__<tool-slug>__<tool-digest>
```

The model sees the server's own title, description and input schema. Tools you switched off in the Tools tab are not offered. If a server fails to connect, its tools are absent for that turn and the failure is logged; the turn itself continues. Subagents inherit the parent's discovered bindings.

A call is translated back to `tools/call` with the tool's original name; the `content`, `structuredContent` and `isError` of the reply are returned to the model as JSON (server `_meta` is stripped). Replies over 64 KiB are refused rather than truncated.

## Approvals

MCP tools are treated as external side effects:

- At `request_approval` and `allow_edits`, every call asks for confirmation unless a `PermissionRequest` hook allows it. At `full_access` calls run without asking.
- A tool that declares `_meta["anthropic/requiresUserInteraction"] = true` asks **on every call**, at every level, and a hook cannot pre-approve it. Its description to the model is prefixed with `mcp.mandatory_description_prefix`. A malformed value fails closed to "ask".
- A tool whose **auto-approve** switch you turned off in the Tools tab behaves the same way: it asks at every level.

## Verifying a server end to end

1. Register it and click **Test connection**; the Tools tab should list its tools and the Logs tab should be quiet (or show the server's own startup messages).
2. Select it in a conversation, send a message that needs one of its tools, and look for a tool card named after the server in the timeline; approve the call when the card appears.
3. If nothing happens: check the system prompt section is present (the server must be selected), then the Logs tab (a stdio server that exits at startup produces no tools), then the model — providers that do not support tool calling never see MCP tools.

## Troubleshooting

| Symptom | Cause |
|---|---|
| Test connection fails with a spawn error | `command` is not on the app's `PATH` or the arguments are wrong. Try the same command in a terminal. On Windows the executable is resolved through `PATH` and `PATHEXT` (so `npx` finds `npx.cmd`), but the command line is not passed through a shell — give an executable name or full path, not a pipeline. |
| Tools appear after a test but the model never calls them | The server is not selected in the conversation, or the provider/model has no tool calling. |
| Every call asks even at Full access | The tool declares `requiresUserInteraction`, or its auto-approve switch is off. |
| Two servers expose the same tool | Names carry a per-server digest, so both are callable; the description tells them apart. |
| An HTTP server refuses `http://` | Only loopback/private addresses may use plain HTTP; public endpoints need `https`. |
