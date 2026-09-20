`preview_stop` kills one dev server this conversation can address and forgets it, buffered output included. The model reaches for it when a server is in the way — it holds a port the next start needs, or the worktree is at its five-server limit — or when the work that server was for is finished. The `serverId` is the one `preview_start` returned or the one `preview_list` reports.

## Approval

Killing a process the project's `launch.json` described is an unbounded action (`tool.unbounded`, high risk): Manual approval, Accept edits and Plan mode ask, Full access does not. **Always allow** can answer the card, remembering the decision for this tool in this conversation. A missing, blank, or non-string `serverId` is refused before any card is drawn.

## Behavior and limits

Only servers registered under this workspace and owned by this conversation, or by none, can be stopped. Any other id — another conversation's server, one that already exited, one that never existed — answers `Server <id> not found`. Success answers `Server <id> stopped`.

The whole process tree is killed, not only the child that was spawned, so a wrapper script cannot leave the real server behind. The server's log ring is dropped with it, so read `preview_logs` first if the output still matters, and the id is retired rather than reused: a later start from the same configuration is numbered instead.

The id an attach entry returned names the conversation's preview page rather than a process, and is refused with a sentence pointing back at `preview_list`. Stopping a server takes its preview page down with it. A server that exits on its own also leaves the list, but its page stays up, because what it then shows — a connection refused, a stack trace printed on the way down — is the diagnosis. Every server is stopped when the app exits.

## Related

- [preview_start](preview_start.html), [preview_list](preview_list.html), [preview_logs](preview_logs.html)
- [Working with Mework](../working.html#the-built-in-browser)
