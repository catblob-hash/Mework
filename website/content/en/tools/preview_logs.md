`preview_logs` returns what a dev server printed: its own stdout and stderr, captured since it started. The model reaches for it after an edit the server has to rebuild, when a page fails to load, or when a started server is not serving what was expected. It is the server's output, not the page's — browser console messages come from `preview_console_logs`.

## Approval

Reading a buffer the host filled itself is a local browser observation (`browser.local_observation`, low risk, read effect), scoped to the workspace and the application's data directory. No security level asks for it, so no approval card ever appears.

## Behavior and limits

Each server keeps a ring buffer of 1000 entries. An entry is one read from the pipe, not one line, so a single entry can hold many lines; both streams share the buffer, in arrival order. `lines` defaults to 50 and is clamped to 1–200 rather than refused; it slices the tail in entries, so much more text can come back.

`level: "error"` keeps only stderr entries containing `error`, `exception`, `failed` or `fatal`, matched case-insensitively; a stdout line saying "error" is not one. `search` is a case-sensitive substring filter applied after the level filter. Any `level` other than `all` or `error` fails the call.

`serverId` and `workspace` resolve as they do for `preview_stop`. Without `serverId` the call falls back to the first starting or running server this conversation may address — in the named workspace, when one is named. Nothing to read is reported in words, not as a failure: `No logs yet.`, `No server errors found.` or `No logs matching "…"`. An id from an attachment, and a call with no server at all, answer with a sentence pointing back at `preview_list`.

A server's buffer is discarded the moment its process exits or `preview_stop` kills it, so output must be read while the server is alive. Servers run with `FORCE_COLOR=1`, so output can carry ANSI escapes.

## Related

- [preview_start](preview_start.html), [preview_list](preview_list.html), [preview_stop](preview_stop.html)
- [preview_console_logs](preview_console_logs.html), [preview_network](preview_network.html)
- [Working with Mework](../working.html#the-built-in-browser)
