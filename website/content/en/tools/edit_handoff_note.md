Replaces one passage of an existing handoff note and refreshes its index line. A conversation that inherited notes and is armed to hand off in turn edits them with this tool rather than writing a second copy, so a chain of handoffs keeps one set of notes up to date.

## Approval

Never asks, at any level: like `create_handoff_note` it changes only this conversation's own notebook (`host.local_state_change`).

## Behavior and limits

`old_text` must occur exactly once in the note; no match or several matches is an error that says which. An empty `new_text` deletes the passage. `description` replaces the note's index line. The edited note is still capped at 256 KiB.

## Related

- [create_handoff_note](create_handoff_note.html), [read_handoff_note](read_handoff_note.html), [handoff](handoff.html)
