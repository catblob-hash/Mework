`read` returns a range of a UTF-8 text file, or attaches an image file as visual context. It is the model's way into any file it is about to change, and it is the gate in front of the writers: `edit` and `write` refuse to touch an existing file this conversation has not read.

## Approval

Calls are classified as reads. Inside the workspace, the app-data directory or a granted extra directory, a read never asks. A file outside those roots asks under `request_approval` and passes under `allow_edits` and `full_access`; **Always allow** remembers that per conversation, capped at the risk level of the card you answered. `memory/` under the app-data directory is refused at every level, `full_access` included.

## Behavior and limits

Text comes back as plain lines, with no line-number prefixes. Without `end_line` a read returns 2,000 lines from `start_line`; a longer range returns its first 5,001 lines. A `0` for either line number counts as not given, and only an `end_line` before `start_line` is refused. Whatever the range, one read returns at most 60 KiB of whole lines; a read that stops before the end of the file or of its range ends with the lines shown, the file's length and the `start_line` to continue from. A single line longer than that cannot be read at all, and the call says to use `grep`. Files over 2 MiB, or bytes that are not UTF-8, are refused. File type comes from content, not extension: a PNG, JPEG, GIF or WebP signature makes it an image read, which ignores the line parameters, takes files up to 32 MiB (5 MiB on a remote machine), 16,384 px per side and 64 MP, and arrives as a numbered `[Image #N]` — shrunk first, as a picture you attach is, to at most 2,000 px a side and about 500 KB.

Any successful read registers the file as read, which is what the writers' gate looks for. A whole-file read — no range, and not cut short — also keeps the text and its modification time; only such a record can clear a stale write by content, or raise the numbered changed-region notice a later request carries once the file has moved on disk. Records are process-local: after a restart, the first edit asks for a fresh read.

## Related

- [edit](edit.html) and [write](write.html) — what the remembered read unlocks
- [grep](grep.html) — find the line before you read around it
- [Working with Mework](../working.html#images) — how images reach the model
