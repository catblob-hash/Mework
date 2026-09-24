`grep` searches text files line by line with a regular expression and returns the hits as `path:line:content`. The model reaches for it to find where a symbol, string or TODO actually lives before opening anything, and to check that a rename left nothing behind. `path` may be a single file or a directory that is searched recursively.

## Approval

Calls are classified as reads. A search that stays inside the workspace, the app-data directory or an extra working directory granted to the conversation runs without asking at every security level, Plan mode included. A path outside those roots asks under `request_approval` and Plan mode, and passes under `allow_edits` and `full_access`; **Always allow** can remember that for this tool in this conversation, capped at the risk level of the card you answered. `memory/` under the app-data directory is refused at every level, `full_access` included.

## Behavior and limits

Patterns use Rust `regex` syntax — character classes, alternation, anchors and repetition, but no backreferences or lookaround — and every line is matched on its own, so `^` and `$` bind to line ends. The walk does not follow symlinks and applies no ignore-file filtering. Files larger than 2 MiB are skipped, and so is any file with a NUL byte among its first 8,192 bytes, which is how binaries are recognised; invalid UTF-8 elsewhere is replaced rather than fatal. Line numbers are 1-based and each reported line is cut at 500 characters. The search stops after 1,000 result lines and says so; an entry that could not be traversed contributes a `[skipped]` line rather than failing the call, and no hits at all answers `No matches found`. Output over 64 KiB is truncated.

## Related

- [find](find.html) — match names instead of contents
- [read](read.html) — open a file at the line a match reported
- [lsp](lsp.html) — definitions and references instead of text matches
- [bash](bash.html) — `rg` when you need flags this tool does not expose
- [find_content](find_content.html) — describe what you are after when no pattern spells it
