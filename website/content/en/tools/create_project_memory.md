Creates one new Markdown document in `<workspace>/.mework/memory/` and records its one-line description in the host-owned `MEMORY.md` index. The project tier travels with the workspace, so the model reaches for this when it learns something that only holds here: how the tests are run, where a service listens.

## Approval

Classified as a host-local state change (`host.local_state_change`, medium risk, write effect). No security level raises a card, and plan mode does not refuse it the way it refuses a file write, so there is no **Always allow** to remember. The mandatory confirmation belongs to the global tier alone.

## Behavior and limits

The body is complete Markdown, up to 256 KiB of UTF-8 and free of NUL characters. The index description has its whitespace collapsed and is capped at 300 characters; it is the only text that reaches `MEMORY.md`, which no tool writes directly. Document names follow the read tool's rules.

An existing name fails instead of overwriting. The body is written to a temporary file in the same directory and published by atomic replace, refusing a symbolic link at the target, and the existence check through the index rewrite holds a blocking cross-process lock (`.memory.lock`) inside the tier's `memory/` directory. A failed index rewrite deletes the new document again, so no orphan body survives. The index itself is capped at 64 KiB.

The tier is unavailable when the conversation is not bound to a workspace directory on disk; it fails closed rather than writing into the global tier. On success the tool reports the normalized file name and the tier. The body stays out of the timeline, hooks and receipts, which keep the name, the description and a byte count.

## Related

- [create_global_memory](create_global_memory.html) — the same write, applied in every workspace
- [edit_project_memory](edit_project_memory.html), [read_project_memory](read_project_memory.html)
- [Working with Mework](../working.html#long-term-memory) — the two tiers and their switches
