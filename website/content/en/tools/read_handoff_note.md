Returns the full text of one handoff note. A conversation that was opened by a handoff has this tool from its first request: the notes it inherited are listed at the end of its system prompt, or right after its first message on a model that reads its system prompt ahead of its tools, and that first message asks it to read them. A conversation armed to hand off has it too, alongside the tools that write the notes.

## Approval

Classified as a read of host state (`host.read_or_coordinate`, low risk). No security level raises a card.

## Behavior and limits

The notes live in a notebook of the conversation's own, `handoffs/<conversation id>/` under the application data directory, which no file tool can reach. `name` is a single leaf name, never a path; the `.md` suffix is optional, and the same rules as memory document names apply (at most 120 characters; no separators, `..`, colon, leading dot, control characters, trailing dot or space, `< > " | ? *` or Windows device names). `HANDOFF.md`, the index the host keeps, is not addressable. A note is at most 256 KiB of UTF-8 and is read through a no-follow handle.

Subagents never get this tool.

## Related

- [create_handoff_note](create_handoff_note.html), [edit_handoff_note](edit_handoff_note.html), [handoff](handoff.html)
- [Working with Mework](../working.html#auto-compact) — auto-compact and handoff
