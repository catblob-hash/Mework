Writes one new handoff note: Markdown for the conversation that will take the work over. The host offers it once the conversation's context crosses the **Auto-compact** threshold; it is never in the tool picker or a preset. The next conversation sees nothing of this one but its system prompt, its tools and these notes, so the notes carry the task, the decisions, what is done, what is left and where the work stopped.

## Approval

Never asks, at any level. The note goes into the conversation's own notebook under the application data directory rather than into the workspace, so it is classified as a change to this conversation's host state (`host.local_state_change`).

## Behavior and limits

`description` becomes the note's line in the notebook's index, `HANDOFF.md`, which the host keeps and rewrites; it is one sentence of at most 300 characters. The body must not be empty and is at most 256 KiB. Creating refuses a name that already exists — change a note with `edit_handoff_note`. Names follow the rules of [read_handoff_note](read_handoff_note.html).

The timeline card shows the note's name, index line and body, so you can read what was handed over.

## Related

- [edit_handoff_note](edit_handoff_note.html), [handoff](handoff.html)
- [Working with Mework](../working.html#auto-compact)
