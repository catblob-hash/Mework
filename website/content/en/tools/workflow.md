`workflow` runs a JavaScript orchestration script that spawns subagents in a shape the model can write down: the same treatment over a list, stages that feed one another, a fixed set of independent checks. The call returns at once — the bare `ok`, or the run's `workflow:<runId>` address when the host had to number the name given — and the run continues as a detached background task that outlives the turn.

## Approval

One outer approval at every level except Full access, classified as an unbounded action at high risk; **Always allow** can remember it for this conversation, capped at that risk. That approval is not a pass for what the script then does: every step is classified again on its own arguments under the same security level, and an isolated step is judged against its own worktree, so a write that escapes it reads as an outside-the-workspace access.

## Behavior and limits

The script may be up to 512 KiB and dispatch at most 1,000 steps, with at most 4,096 items in any array crossing the script boundary. Steps run in the run's own pool and do not consume the conversation's `agent_spawn` budget. A step that goes 180 seconds without progress is retried, up to five times; the run as a whole is bounded at 30 minutes. When the script returns, steps still running are cancelled. Scripts are single bodies with no imports and no host bindings beyond `agent()`, `parallel()`, `pipeline()`, `phase()`, `log()`, `budget` and `args`; `Date.now()`, argless `new Date()` and `Math.random()` throw, because a resume replays the script. Each run keeps a directory under the app-data folder with the approved `script.js`, an append-only journal and one file per step; `resume_run_id` reuses journaled results and re-runs from the first step that misses, and a resubmitted script must match the approved bytes exactly.

## Related

- [Working with Mework](../working.html#subagents-workflows-and-tasks) — tasks, roles, worktrees and how results come back.
- [agent_spawn](agent_spawn.html), [task_wait](task_wait.html), [task_list](task_list.html).
