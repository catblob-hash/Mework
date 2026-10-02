`preview_list` reports the dev servers running in this conversation's workspaces that it is allowed to address. The model reaches for it to find a `serverId` before `preview_stop` or `preview_logs`, and to check whether the server it wants is already up before calling `preview_start` again.

## Approval

Reading the host's own server registry is a local browser observation (`browser.local_observation`, low risk, read effect), scoped to the workspace and the application's data directory. No security level asks for it, so no approval card ever appears.

## Behavior and limits

The result is a pretty-printed JSON array, one object per server, carrying `serverId`, `port`, `status` and `startedAt`, plus `workspace` when the conversation has more than one and `machine` for a server on another machine. `serverId` is the entry's name in `launch.json`, numbered `-1`, `-2`, … when the file repeats a name; two workspaces can both have a `dev`, and `workspace` is what tells them apart. `status` is `starting` until the readiness poll ends and `running` from then on; those are the only two values a listing shows, because the registry drops a server the moment its process exits, whoever ended it. An id absent here has either gone already or never existed.

Servers are listed workspace by workspace, each in the order they started, and only those this conversation started or that no conversation owns. Another conversation's servers stay invisible here even though they count towards the limit of five servers per worktree. An attach entry — a `url` with no command — has no process behind it and is never listed: `preview_start`'s own receipt is the only report of an attachment, and the browser pane lists those from the configuration file.

The same id addresses the server as a background task, spelled `preview:<serverId>` — `preview:<serverId>@<workspace>` with more than one workspace — which is the form `task_list` prints and `task_wait` accepts.

## Related

- [preview_start](preview_start.html), [preview_stop](preview_stop.html), [preview_logs](preview_logs.html)
- [task_list](task_list.html), [task_wait](task_wait.html)
- [Working with Mework](../working.html#the-built-in-browser)
