`preview_stop` kills one dev server this conversation can address and forgets it, buffered output included. The model reaches for it when a server is in the way — it holds a port the next start needs, or the worktree is at its five-server limit — or when the work that server was for is finished. The `serverId` is the server's name in `launch.json` — numbered for a repeated name — as `preview_list` reports it.

## Approval

Killing a process the project's `launch.json` described is an unbounded action (`tool.unbounded`, high risk): Manual approval and Accept edits ask, Full access does not. **Always allow** can answer the card, remembering the decision for this tool in this conversation. A missing, blank, or non-string `serverId` is refused before any card is drawn.

## Behavior and limits

Only servers in this conversation's workspaces that it started, or that no conversation owns, can be stopped. Any other id — another conversation's server, one that already exited, one that never existed — answers `Server <id> not found`. Success answers `Server <id> stopped`.

Two workspaces can each run a `dev`, so with more than one workspace `workspace` completes the address. It can be left out whenever only one workspace runs a server with that id; a call that leaves it out when several do is refused with their numbers, and one naming a workspace that runs no such server is refused with the numbers of those that do.

The whole process tree is killed, not only the child that was spawned, so a wrapper script cannot leave the real server behind. The server's log ring is dropped with it, so read `preview_logs` first if the output still matters. Starting the same entry again brings it back under the same id.

An attach entry's id names no process, and is refused with a sentence saying there is nothing to stop. Stopping a server takes its preview page down with it. A server that exits on its own also leaves the list, but its page stays up, because what it then shows — a connection refused, a stack trace printed on the way down — is the diagnosis. Every server is stopped when the app exits.

## Related

- [preview_start](preview_start.html), [preview_list](preview_list.html), [preview_logs](preview_logs.html)
- [Working with Mework](../working.html#the-built-in-browser)
