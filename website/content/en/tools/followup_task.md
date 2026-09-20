`followup_task` is how a child agent gets another turn. Where `send_message` only adds to what a child will read, this one guarantees the child runs again — so it is the call that reopens a child which has already answered, or redirects one that is between turns.

## Approval

It changes host state inside this conversation and nothing else (`host.local_state_change`), so no security level asks about it. What the child then does is classified call by call under the conversation's security level, exactly as during its first turn.

## Behavior and limits

A child that is idle, failed or finished is woken at once and continues with its full history intact. A running child has the instruction queued and picks it up once its current turn ends. `message` is capped at 32768 characters, and `target` must be a child agent — a workflow run or a background shell command refuses it and points at `task_wait`.

Waking needs a free slot in the conversation's pool of eight live agents. Past that cap the instruction is still queued, but the wake is refused and the result says to collect a finished child with `task_wait` first, then send again.

A child that finished in an earlier turn is restored from the conversation's own record, so continuing it across turns works for as long as its role definition and any conversation-fork binding still resolve unchanged. `task_list` marks such rows resumable. A row marked view-only is one whose definition was replaced, revised or had its memory scope changed: it cannot be continued, and its recorded result stays readable in the transcript.

## Related

- [agent_spawn](agent_spawn.html) — creates the child
- [send_message](send_message.html) — queue without starting a turn
- [task_wait](task_wait.html), [task_list](task_list.html), [box](box.html)
- [Working with Mework](../working.html#subagents-workflows-and-tasks)
