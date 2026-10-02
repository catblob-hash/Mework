Replaces one passage of an existing document in `~/.mework/memory/` and refreshes that document's line in the `MEMORY.md` index. The model reaches for this when a fact it stored earlier has changed: it supplies the old passage and the new one, not a rewritten file. The rest of the document is untouched.

## Approval

Every call raises a confirmation card, at every security level, full access included (`memory.global_persistent_mutation`, high risk, write effect). The prompt is mandatory: a `PreToolUse` hook answering `allow` does not satisfy it, the card offers no **Always allow**, and a standing allowance recorded earlier for this tool name is neither consulted nor written. Declining fails the call and leaves the turn running.

## Behavior and limits

The passage to replace must be non-empty and match exactly once. Two matches are an error that names the count and asks for a longer unique match; zero matches is an error too. Replacement text identical to the original is refused as a no-op, while empty replacement text deletes the passage. A description is required on every edit and replaces the document's index line — up to 300 characters, whitespace collapsed. The resulting body is re-checked against the 256 KiB limit.

The index is rewritten before the body, so a failure there leaves the old description and the old body both in place. The whole read-modify-write holds the tier's blocking cross-process lock (`.memory.lock`). The document must already exist; there is no create-on-edit, and no tool deletes a document — the six memory tools are read, create and edit, per tier. On success the tool reports the file name and the tier without echoing the new body. Like any other tool's, the call — both passages included — appears on the timeline card, reaches hooks, and stays in the conversation the model is replayed.

## Related

- [edit_project_memory](edit_project_memory.html) — the same edit, scoped to the workspace
- [create_global_memory](create_global_memory.html), [read_global_memory](read_global_memory.html)
- [Working with Mework](../working.html#long-term-memory) · [Hooks](../hooks.html)
