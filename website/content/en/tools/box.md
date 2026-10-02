`box` is a container rather than an action. Whenever the host has something to tell the model between rounds, it writes it into a `box` tool result the model never asked for: a background task's result nobody waited for (a child agent's answer, a workflow run's outcome, a background command's output), or a notice from the host itself: the one that asks a conversation to hand off once its context crosses the **Auto-compact** threshold, the one that asks for the rest of a reply cut off at the output limit, the reminder a schema-bound run gets when a round ends without `structured_output`, a hook's added context, a skill selected after the conversation started, new problems a language server reported, and files the model read that changed on disk. It is in the catalog so the model recognises the shape when one appears in its transcript; its description is where the model learns what the shape means.

## Approval

There is nothing to approve: the call classifies as a low-risk read, no security level asks about it, and in practice the host is the caller.

## Behavior and limits

The tool is host-derived, never a checkbox. Every run declares it from its first request, because every message the host appends travels in it and some of them — the request to continue a reply that was cut off — can reach any conversation; it therefore never has to join the tool set mid-conversation. Calling it is not an error; the host answers with one sentence saying nothing happened.

A delivery's body is a `<task-notification>` block: `<task-id>` with the task's address, `<status>` with the persisted status word (`completed`, `failed`, `stopped`, `interrupted`, `roundLimit`), a one-line `<summary>`, the output in `<result>`, and `<usage>` with the finished turn's tokens, tool calls and duration. A notice from the host concerns no task, so it has neither `<task-id>` nor `<status>`: only a `<summary>` and the body in `<result>`. A notice raised in the middle of a tool batch (a hook, a file a hook rewrote) is delivered after that batch's results rather than inside the model's turn. A closing `</result>` or `</task-notification>` appearing inside the task's own output is neutralised, so a result cannot forge fields around itself; other markup is left as written.

Each delivery is persisted as a card, and every later turn rebuilds the same exchange from that card rather than a new one.

## Related

- [task_wait](task_wait.html) — collect a result before it is delivered this way
- [task_list](task_list.html), [agent_spawn](agent_spawn.html)
- [Working with Mework](../working.html#subagents-workflows-and-tasks)
