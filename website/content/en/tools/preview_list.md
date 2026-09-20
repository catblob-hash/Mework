`preview_list` reports the dev servers running for this workspace that this conversation is allowed to address. The model reaches for it to find a `serverId` before `preview_stop` or `preview_logs`, and to check whether the server it wants is already up before calling `preview_start` again.

## Approval

Reading the host's own server registry is a local browser observation (`browser.local_observation`, low risk, read effect), scoped to the workspace and the application's data directory. No security level asks for it, Plan mode included, so no approval card ever appears.

## Behavior and limits

The result is a pretty-printed JSON array, one object per server, carrying `serverId`, `name`, `port`, `status`, `startedAt`, `cwd` and `sessionId`. `cwd` is the worktree the server is registered under, not the process's own directory. `status` is `starting` until the readiness poll ends and `running` from then on; those are the only two values a listing shows, because the registry drops a server the moment its process exits, whoever ended it. An id absent here has either gone already or never existed.

Only this workspace's servers are listed, and only those this conversation started or that no conversation owns. Another conversation's servers stay invisible here even though they count towards the limit of five servers per worktree. An attach entry — a `url` with no command — has no process behind it and is never listed: `preview_start`'s own receipt is the only report of an attachment, and the browser pane lists those from the configuration file.

The same id addresses the server as a background task, spelled `preview:<serverId>`, which is the form `task_list` prints and `task_wait` accepts.

## Related

- [preview_start](preview_start.html), [preview_stop](preview_stop.html), [preview_logs](preview_logs.html)
- [task_list](task_list.html), [task_wait](task_wait.html)
- [Working with Mework](../working.html#the-built-in-browser)
