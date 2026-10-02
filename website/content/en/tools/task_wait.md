`task_wait` is the model's only blocking call. Everything a conversation starts in the background — a spawned child, a workflow run, a command sent to the background — reports back through it, so the model reaches for it when it cannot write the next sentence without the answer.

## Approval

Waiting reads the host's own task table and touches nothing else, so it classifies as a low-risk read and no security level asks about it.

## Behavior and limits

One call may name at most 16 addresses, in the forms `task_list` prints: a child agent by its bare name, `workflow:<runId>` (the bare run name resolves too), `shell:<id>`, `terminal:<id>` and `preview:<serverId>` — `preview:<serverId>@<workspace>` when the conversation has several workspaces. A dev server's bare `preview:<serverId>` is accepted there too while only one workspace runs a server with that id; when several do, the call is refused with each full address. An address nothing answers to fails the call and points at `task_list`. Omitting `tasks` watches every child agent, workflow run and background command of the conversation; terminals and dev servers are left out. `timeout_seconds` runs from 5 to 600 — a value outside is held to the nearer end — and defaults to 60, which is also what `0` means.

Each result comes back under a `[name · status]` header carrying the child's text, its structured value when the spawn set a `schema`, and a closing line of tokens, tool calls and milliseconds. The answer is capped at 64 KiB, and that budget is divided across the results before anything else is added, so one long result cannot push the others out. Progress updates follow the results rather than preceding them, and a closing status list names where every watched task stands. Terminals and dev servers hand back no result; their state is reported instead. On the deadline the answer names both what has arrived and what is still running, and says that nothing was lost.

## Related

- [agent_spawn](agent_spawn.html) — starts the children whose results this collects
- [task_list](task_list.html) — the addresses this tool accepts
- [box](box.html) — how an uncollected result arrives instead
- [Working with Mework](../working.html#subagents-workflows-and-tasks)
