`write` creates a file or replaces its whole content in one call. The model reaches for it for new files and for rewrites large enough that patching would be noise; `edit` is the smaller instrument for a change inside an existing file. Parent directories are created as needed.

## Approval

Calls are classified as writes. Inside the workspace, the app-data directory or a granted extra directory, `request_approval` asks every time; `allow_edits` and `full_access` allow it. A target outside those roots asks under `allow_edits` as well, a write aimed at the app's own data document (`document.v1.json` in the app-data directory) still asks at that level, and `memory/` under the app-data directory is refused at every level, `full_access` included. **Always allow** is capped at the risk level of the card you answered.

## Behavior and limits

Content is at most 2 MiB of UTF-8, and an empty string is a valid file. The write goes through a temporary file and an atomic replace, so an interrupted call never leaves half a file. `..`, symlinks and junctions cannot leave the authorized scope: the boundary is checked on the canonical path, for a new file on its nearest existing ancestor. In plan mode a target that is part of a Git repository — tracked, or a new file Git would not ignore — is refused until plan mode ends ([Plan mode](../working.html#plan-mode)). An existing target must have been read in this conversation first, and the write is refused when the file's modification time is newer than that read, unless the text on disk is still the text the model saw. The result is a short receipt; the tool card shows the change as a unified diff, truncated past 64 KiB. A `PostToolUse` hook that blocks the call afterwards does not undo the bytes: the receipt says it ran and only its result was rejected.

## Related

- [edit](edit.html) — replace one passage instead
- [read](read.html) — the prior read this tool requires
- [Working with Mework](../working.html#tools-and-approvals) — the security levels
- [Hooks](../hooks.html) — rewriting or blocking a write
