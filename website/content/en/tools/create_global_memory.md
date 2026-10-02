Creates one new Markdown document in `~/.mework/memory/` and records its one-line description in the host-owned `MEMORY.md` index. The global tier is loaded in every workspace, so the model reaches for this when it learns something durable that is not tied to the open project: a preference, a machine fact, a convention.

## Approval

Every call raises a confirmation card, at every security level, full access included (`memory.global_persistent_mutation`, high risk, write effect). The prompt is mandatory: a `PreToolUse` hook answering `allow` does not satisfy it, the card offers no **Always allow**, and a standing allowance recorded earlier for this tool name is neither consulted nor written. Declining fails the call and leaves the turn running.

## Behavior and limits

The body is complete Markdown, up to 256 KiB of UTF-8 and free of NUL characters. The index description has its whitespace collapsed and is capped at 300 characters; it is the only text that reaches `MEMORY.md`, which no tool writes directly. Document names follow the read tool's rules.

An existing name fails instead of overwriting. The body is written to a temporary file in the same directory and published by atomic replace, refusing a symbolic link at the target, and the existence check through the index rewrite holds a blocking cross-process lock (`.memory.lock`), so a second Mework instance sharing `~/.mework` waits rather than losing an index update. A failed index rewrite deletes the new document again, so no orphan body survives. The index itself is capped at 64 KiB. On success the tool reports the normalized file name and the tier. Like any other tool's, the call — body included — appears on the timeline card, reaches hooks, and stays in the conversation the model is replayed.

## Related

- [create_project_memory](create_project_memory.html) — the same write, scoped to the workspace
- [edit_global_memory](edit_global_memory.html), [read_global_memory](read_global_memory.html)
- [Working with Mework](../working.html#long-term-memory) · [Hooks](../hooks.html)
