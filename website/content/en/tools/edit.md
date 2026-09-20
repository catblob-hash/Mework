`edit` replaces one exact passage of an existing file and leaves everything else byte for byte. It is the model's normal way to change code it has read, and it fails loudly when the file is not what the model believed.

## Approval

Calls are classified as writes: `request_approval` asks every time; `allow_edits` and `full_access` allow an edit inside the workspace, the app-data directory or a granted extra directory; a file outside those roots asks under `allow_edits` too; Plan mode refuses it with an error rather than a card. `memory/` under the app-data directory is refused at every level, `full_access` included. **Always allow** is capped at the risk level of the card you answered.

## Behavior and limits

The target must already exist — `edit` never creates a file — be UTF-8, and come to at most 2 MiB after the replacement. The search text must occur exactly once: several matches are refused with the count, and no match at all is its own error. The raw text is tried first, so a file with mixed line endings keeps every byte outside the replaced span. If that finds nothing, the match is retried with the byte-order mark removed and CRLF folded to LF on both sides, and the result written back in the file's own convention — CRLF when CRLF wins the vote over the first 4,096 characters, the mark restored when the file had one.

A prior `read` in this conversation is required. If the file changed on disk since then, the edit still applies when the search text is still there exactly once, and the receipt warns that the file holds changes the model has not seen. The write is atomic and the tool card shows a diff; a `PostToolUse` hook that blocks afterwards does not undo the bytes.

## Related

- [write](write.html) — replace the whole file
- [read](read.html) — the prior read it requires
- [Working with Mework](../working.html#tools-and-approvals) — the security levels
- [Hooks](../hooks.html) — formatters that rewrite the file
