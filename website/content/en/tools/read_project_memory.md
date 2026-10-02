Returns the full text of one document from the open workspace's memory directory, `<workspace>/.mework/memory/`. With the project memory switch on, that tier's `MEWORK.md` and `MEMORY.md` index travel with the request; the index carries names and one-line descriptions, not bodies. The model calls it when an index line looks relevant.

## Approval

Classified as a read of host state (`host.read_or_coordinate`, low risk, read effect). No security level raises a card, so there is no **Always allow** to remember.

## Behavior and limits

`name` is a single leaf name inside the memory directory, never a path. The `.md` suffix is optional and matched case-insensitively, and the whole name, suffix included, is capped at 120 characters. Path separators, `..`, a colon, a leading dot, control characters, a trailing dot or space, `< > " | ? *` and Windows device names (`CON`, `NUL`, `COM1`) are rejected rather than sanitized. `MEMORY.md` is not addressable under any spelling: the host owns the index and rewrites it from the descriptions the create and edit tools carry.

The file is opened through a no-follow handle — a symbolic link or reparse point at the path fails rather than being followed — and must be valid UTF-8 and at most 256 KiB. The model receives the body verbatim, and like any other tool's result it stays on the timeline card and in the conversation the model is replayed. A missing document reports the tier searched, without disclosing a path.

The two tiers are independent: the same name may exist in both. The tier is unavailable when the conversation is not bound to a workspace directory on disk — it fails closed instead of falling back to the global tier — and refused outright when the project memory switch is off.

## Related

- [read_global_memory](read_global_memory.html) — the same read against the machine-wide tier
- [create_project_memory](create_project_memory.html), [edit_project_memory](edit_project_memory.html)
- [Working with Mework](../working.html#long-term-memory) — the two tiers and their switches
