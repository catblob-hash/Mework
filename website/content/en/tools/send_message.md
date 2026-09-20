`send_message` drops a line into a child agent's inbox. Nothing else happens: no turn starts, no result is collected, and the child reads the line the next time it assembles a model request. The model uses it to correct a child that is already running — a file name it has just learned, a constraint that only now matters.

## Approval

The call coordinates inside this conversation and touches no file, shell or network, so it classifies as a low-risk read (`host.read_or_coordinate`) and no security level asks about it.

## Behavior and limits

`target` must be a child agent of this conversation; one that finished in an earlier turn is restored from the conversation's own record first. A workflow run or a background shell command is refused, and the refusal points at `task_wait`. `message` is capped at 32768 characters.

The result says which of two things happened, because the difference decides what to do next. A running child has the message delivered and reads it during its current turn. A child that is not running only has it queued — and a queued message wakes nobody, so `followup_task` is what starts another turn, and it carries the queued messages with it.

Children hold a narrowed form of the tool that takes no `target`, because their only recipient is the main agent that spawned them. They receive it only when the parent conversation has both `agent_spawn` and `send_message` enabled. Up to 32 such messages are held per child, oldest dropped first, and they reach the main agent at its next round boundary as a `box` delivery with the status `message`.

## Related

- [agent_spawn](agent_spawn.html) — creates the child and its name
- [followup_task](followup_task.html) — append an instruction and start a turn
- [task_wait](task_wait.html), [task_list](task_list.html), [box](box.html)
- [Working with Mework](../working.html#subagents-workflows-and-tasks)
