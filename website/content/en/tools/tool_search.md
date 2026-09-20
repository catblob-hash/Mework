With **Tool discovery** on, a conversation's MCP tools are announced by name only and their schemas are held back; `tool_search` is how the model gets the schema of the one it needs. It takes a query, matches it against the withheld list, and returns the matched tools' full JSONSchema definitions in the same `<functions>` encoding the declared tool set uses.

## Approval

No security level asks. The schemas were discovered at the start of the turn and are already in the request, so handing one out touches nothing outside the run. The MCP tool itself is a separate matter: calling it is classified and approved on its own, as any MCP call is.

## Behavior and limits

The tool appears only when discovery is on and the run withheld at least one tool. A `select:` query resolves exact names, comma-separated and case-insensitively, and returns every one it finds; a name it cannot resolve is simply absent, and a query that resolves nothing at all answers with how many tools are deferred and where their names are listed. Any other query is a keyword search over the withheld names, their servers and their descriptions, with an MCP name split into server and tool words so either half can match. Once a schema has been handed out, that tool is declared like any other from the next step onward and is called directly — no second search. The announcement of withheld names is fixed for the whole run and does not shrink as schemas arrive. A subagent dials the same servers and withholds the same tools; it starts with none of the parent's fetches. The switch itself freezes once a conversation has dialed a server, because the transcript already carries its tools one way.

## Related

- [MCP servers](../mcp.html#how-mcp-tools-reach-the-model) — naming, discovery and the approval rules for the tools this hands out.
- [skill](skill.html) — the same on-demand shape, for skill bodies.
