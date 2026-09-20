`todo` is the conversation's own task list: the model creates entries for the work it has agreed to do, moves them through `pending` → `in_progress` → `completed`, and can record which entry blocks which. The list is drawn at the top of the Tasks panel as a collapsible plan with a progress bar, a done-of-total count and the active entry's present-continuous wording. It is bookkeeping the user can read, not a scheduler — nothing runs because an entry exists.

## Approval

No security level asks. `get` and `list` are classified as reads of host state; `create` and `update` as a change to this conversation's own state — neither reaches a file, a shell or the network. An `action` the host cannot read is classified as the write, which is the stricter of the two. The tool is scheduled only by the model run loop and cannot be executed by hand.

## Behavior and limits

The list is rebuilt at the start of each run by replaying this conversation's successful `todo` results, so it follows the transcript: delete or edit those messages and the list changes with them. IDs are minted in order as `task-1`, `task-2`, …, and a list holds at most 1,000 entries. A task may not block or be blocked by itself, every related ID must already exist, and dependency cycles are refused. A task still blocked by an unfinished one cannot become `in_progress` or `completed` — only `pending` is legal while blocked. `status: "deleted"` removes the entry and every reference to it from the other entries. Reading an ID that does not exist answers with a null task rather than failing. Subagents never get this tool.

## Related

- [Working with Mework](../working.html#subagents-workflows-and-tasks) — the Tasks panel and what else appears in it.
- [task_list](task_list.html) — the running background tasks, which is a different list.
