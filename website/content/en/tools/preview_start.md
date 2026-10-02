`preview_start` runs one of the dev servers described in the workspace's `.mework/launch.json` and points this conversation's preview page at it. The model reaches for it before verifying its work: every page tool acts on the page this one opens. An entry naming a `url` and no command attaches to a server something else runs; nothing is spawned.

## Approval

Running a command the project's `launch.json` names is an unbounded action (`tool.unbounded`, high risk): Manual approval and Accept edits raise a card, Full access does not. **Always allow** can answer it, remembering the decision for this tool in this conversation. A missing, blank, or non-string `name` is refused before any card is drawn.

## Behavior and limits

A missing or broken `launch.json` is answered with what is wrong and the expected file format. Names match case-insensitively, and a file holding one usable entry and nothing malformed accepts any name; an unknown name otherwise comes back with the names that do exist.

A server already running under the resolved entry's id — this conversation's own, or one nobody owns — is handed back and no second process started. A worktree runs at most five servers, other conversations' included. Spawning is attempted up to three times, except for `ENOENT`, `EPERM`, `EACCES` and `ENOTDIR`, reported at once.

With `autoPort: true` a taken port is swapped for a free one and `--port` and `-p` arguments rewritten to match; otherwise the conflict is reported, naming the occupant where possible. The call returns after a three-second startup gate — an exit inside it is a failure, silence is success — and readiness is then polled for up to 60 seconds, which can only move the row to `running`.

A server's `serverId` is its entry's name as the file writes it. Nothing stops a file from repeating a name, so a repeated one is numbered: its entries answer to `<name>-1`, `<name>-2`, … in the order each is first started in that workspace, and keep the number when started again. The bare name starts the first of them not running yet, or hands back the one numbered 1 once every one is; a numbered id addresses its entry. Each name counts on its own, and so does each workspace.

With more than one workspace, `workspace` names whose `launch.json` is read and so on which machine the server runs; it defaults to 1. For a workspace on an SSH machine the server is started by the agent there and readiness is waited out there; the conversation's page moves onto that machine's network, so `http://localhost:<port>` in the page is the server on that machine.

The result is a sentence or two rather than a snapshot: whether a process was started or an existing one reused, the port when it is not the configured one, and where the page opened — at `http://localhost:<port>` or the entry's `url`. The id is not repeated back, since it is the name the call passed, unless the name is a repeated one: then the result leads with the numbered id. The server is also a task, addressed as `preview:<serverId>`.

## Related

- [preview_list](preview_list.html), [preview_logs](preview_logs.html), [preview_stop](preview_stop.html)
- [Working with Mework](../working.html#the-built-in-browser)
