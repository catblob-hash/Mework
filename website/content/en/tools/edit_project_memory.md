Replaces one passage of an existing document in `<workspace>/.mework/memory/` and refreshes that document's line in the `MEMORY.md` index. The model reaches for this when something it recorded about this project has changed, and supplies the old passage with its replacement rather than a rewritten file.

## Approval

Classified as a host-local state change (`host.local_state_change`, medium risk, write effect). No security level raises a card, so there is no **Always allow** to remember. The mandatory confirmation belongs to the global tier alone.

## Behavior and limits

The passage to replace must be non-empty and match exactly once. Two matches are an error that names the count and asks for a longer unique match; zero matches is an error too. Replacement text identical to the original is refused as a no-op, while empty replacement text deletes the passage. A description is required on every edit and replaces the document's index line — up to 300 characters, whitespace collapsed. The resulting body is re-checked against the 256 KiB limit.

The index is rewritten before the body, so a failure there leaves the old description and the old body both in place. The whole read-modify-write holds the tier's blocking cross-process lock (`.memory.lock`). The document must already exist; there is no create-on-edit, and no tool deletes a document — the six memory tools are read, create and edit, per tier. The tier is unavailable when the conversation is not bound to a workspace directory on disk. On success the tool reports the file name and the tier without echoing the new body. Like any other tool's, the call — both passages included — appears on the timeline card, reaches hooks, and stays in the conversation the model is replayed.

## Related

- [edit_global_memory](edit_global_memory.html) — the same edit, applied in every workspace
- [create_project_memory](create_project_memory.html), [read_project_memory](read_project_memory.html)
- [Working with Mework](../working.html#long-term-memory) — the two tiers and their switches
