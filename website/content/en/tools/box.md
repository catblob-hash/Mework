`box` is a container rather than an action. When a background task reaches a terminal state and nobody waited for it, the host writes that task's result into a `box` tool result the model never asked for — a child agent's answer, a workflow run's outcome, a background command's output. It is in the catalog so the model recognises the shape when one appears in its transcript.

## Approval

There is nothing to approve: the call classifies as a low-risk read, no security level asks about it, and in practice the host is the caller.

## Behavior and limits

The tool is host-derived, never a checkbox. It appears as soon as the conversation enables `agent_spawn`, `workflow`, `bash`, `powershell` or `preview_start` — the five tools that can put a row in the task list — and goes away with them. Calling it is not an error; the host answers with one sentence saying nothing happened.

A delivery's body is a `<task-notification>` block: `<task-id>` with the task's address, `<status>` with the persisted status word (`completed`, `failed`, `stopped`, `interrupted`, `roundLimit`), a one-line `<summary>`, the output in `<result>`, and `<usage>` with the finished turn's tokens, tool calls and duration. Messages a still-running child sends up to the main agent ride the same carrier, with the status `message` and no `<usage>`. A closing `</result>` or `</task-notification>` appearing inside the task's own output is neutralised, so a result cannot forge fields around itself; other markup is left as written.

Each delivery is persisted as a card, and every later turn rebuilds the same exchange from that card rather than a new one.

## Related

- [task_wait](task_wait.html) — collect a result before it is delivered this way
- [task_list](task_list.html), [agent_spawn](agent_spawn.html)
- [Working with Mework](../working.html#subagents-workflows-and-tasks)
