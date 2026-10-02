Hands the conversation off: opens a new conversation that continues the work from the handoff notes, starts it, and ends the current run. It takes no arguments — the notes are the whole handoff. The host offers it once the conversation's context crosses the **Auto-compact** threshold.

## Approval

Never asks, at any level (`host.local_state_change`). The new conversation has exactly this conversation's settings, so it grants nothing that was not already granted here, and a card at the moment the context runs out would stall the turn it serves.

## Behavior and limits

The call is refused while the notebook is empty, and while background agents or workflows are still running for this conversation — their results come back here, so the model waits for them (`task_wait`) or stops them first. A background shell command does not hold it up.

On success the new conversation is created as a fork of this one (named `<origin>-fork-N`, in the same place in the sidebar) and its first run starts at once; if this conversation is on screen, the page follows it. Its timeline is this conversation's system prompt (its first card, when that is a system card — one anywhere else is not carried over), a system card listing the inherited notes — which reaches the model as the last section of its system prompt, right after that prompt — and one user message, the prompt profile's `handoff.start_message` (by default: read the handoff notes, then continue the work). That is on a model that reads its tools ahead of its system prompt; on any other the index card is left out and the first run hands the index over right after the opening message instead, as a mid-conversation system message or, where the model and endpoint take none, in `box`. It gets a copy of the notebook, so it can read the notes and, when it is armed in turn, edit them. The current run stops right after the call: tool calls after it in the same round are skipped, and nothing is sent back to the model. The original conversation keeps its whole timeline.

## Related

- [create_handoff_note](create_handoff_note.html), [read_handoff_note](read_handoff_note.html)
- [fork](fork.html) — a fork the model requests and you approve
- [Working with Mework](../working.html#auto-compact)
