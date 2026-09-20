`read` returns a range of a UTF-8 text file, or attaches an image file as visual context. It is the model's way into any file it is about to change, and it is the gate in front of the writers: `edit` and `write` refuse to touch an existing file this conversation has not read.

## Approval

Calls are classified as reads. Inside the workspace, the app-data directory or a granted extra directory, a read never asks — Plan mode is read-only, not blind. A file outside those roots asks under `request_approval` and Plan mode and passes under `allow_edits` and `full_access`; **Always allow** remembers that per conversation, capped at the risk level of the card you answered. `memory/` under the app-data directory is refused at every level, `full_access` included.

## Behavior and limits

Text comes back as plain lines, with no line-number prefixes. A call covers at most 5,001 lines, and without `end_line` the read stops there and says so. Files over 2 MiB, or bytes that are not UTF-8, are refused. File type comes from content, not extension: a PNG, JPEG, GIF or WebP signature makes it an image read, which rejects the line parameters, caps at 5 MiB, 8,000 px per side and 16 MP, and arrives as a numbered `[Image #N]`.

Any successful read registers the file as read, which is what the writers' gate looks for. A whole-file read — no range, and not cut by the line cap — also keeps the text and its modification time; only such a record can clear a stale write by content, or raise the numbered changed-region notice a later request carries once the file has moved on disk. Records are process-local: after a restart, the first edit asks for a fresh read.

## Related

- [edit](edit.html) and [write](write.html) — what the remembered read unlocks
- [grep](grep.html) — find the line before you read around it
- [Working with Mework](../working.html#images) — how images reach the model
