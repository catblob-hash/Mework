A subagent is a second agent in the same workspace, with its own context window, transcript and row in the task panel. `agent_spawn` creates one, hands it a task text and lets the parent's turn carry on. The model reaches for one when a strand of work would otherwise crowd its own context with findings it will not reuse.

## Approval

Delegation classifies as a write under the rule `agent.delegation`: Manual raises an approval card, Accept edits and Full access dispatch without one. **Always allow** can remember it for the conversation, and a permission hook may answer in your place. Approving covers the delegation only: the child inherits the conversation's security level and each tool it calls is classified again.

## Behavior and limits

The task text is the only instruction a child ever gets. It runs on its own until it answers; nothing can message it, redirect it or hand it another task, whether it is still running or has finished. Its answer comes back through `task_wait`; if nobody waits for it, the host delivers it to the parent as a `box` result once the child finishes. More work means a new child.

A name is reserved for the whole conversation branch tree: one any child has already used is refused permanently, even after that child finished. Eight children may run at once; a ninth is refused until `task_wait` collects one. `prompt` is capped at 32768 characters.

A child runs with the parent's tools minus everything that would open something of its own: `agent_spawn`, `workflow`, `fork`, `ask_user` and the plan tools are absent, and the parent's task board is stripped and re-derived against the child's own tasks. It always gains a progress-report tool. When the conversation defines roles and does not allow roleless subagents, `agent_type` is required and `context: "conversation"` is refused; `none`, the default, is accepted. A `schema` is compiled at this call, so an unsupported keyword is an argument error here rather than a failure inside the child. The receipt is the bare word `ok`.

## Related

- [task_wait](task_wait.html), [task_list](task_list.html), [box](box.html) — collect and inspect
- [workflow](workflow.html) — scripted fan-out instead of delegation by hand
- [Working with Mework](../working.html#subagents-workflows-and-tasks)
