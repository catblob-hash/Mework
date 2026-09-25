`task_list` is the conversation's task board as the model sees it: child agents, workflow runs, terminals, dev servers and background commands, running or long finished, each with the address it answers to. The model reaches for it before waiting on something, after picking a conversation back up, or when it needs to know whether a finished child can still be continued.

## Approval

It reads host state and nothing else, so no security level asks about it.

## Behavior and limits

A count leads the listing, and rows are grouped into subagents, workflows, terminals, dev servers and shell commands. Each row prints the address `task_wait` takes back — a bare agent name, `workflow:<runId>`, `terminal:<id>`, `preview:<serverId>` or `shell:<id>` — then the status, the display label when it differs from the address, a preview of the row's detail compacted to 120 characters (a child's task text, a terminal's shell and working directory, a dev server's `localhost:<port>` and, for one on another machine, that machine, the command a shell row ran), and the latest progress update if the task sent one.

Dev servers are listed from every workspace of the conversation. Two workspaces can both run a `dev`, so with more than one workspace a dev server's address carries its workspace number: `preview:dev@2`.

A finished child is marked resumable when `followup_task` can still continue it. One rebuilt from the transcript of an earlier turn is marked view-only instead when its role definition or fork binding no longer resolves. Workflow runs and shell commands are never continuable, and a workflow run that has left the pool says its result is in the timeline. Finished commands stay listed with their exit code, so the list answers whether that build passed, not only what is running.

A conversation with no tasks at all gets one line saying so. The whole listing is capped at 64 KiB.

## Related

- [task_wait](task_wait.html) — takes the addresses printed here
- [agent_spawn](agent_spawn.html), [followup_task](followup_task.html)
- [box](box.html) — uncollected results arrive on their own
- [Working with Mework](../working.html#subagents-workflows-and-tasks)
