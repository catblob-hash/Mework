The `lsp` tool asks a language server about **symbols** instead of text: where a name is defined, who references it, what its type is, who calls this function. Mework speaks the Language Server Protocol to the same servers your editor uses. The model reaches for it when the question is about a declaration or a caller rather than a string.

## Approval

An ordinary call is classified as a **read** of `filePath`: it passes at every level inside the workspace, and asks at Manual approval when the file sits outside it. If the workspace ships its own `.mework/lsp.json`, every call is classified as an **unbounded action** instead and asks at every level below Full access — that file names the command Mework launches, and only its existence is checked, never its contents. Where the workspace has no such file, a server from `~/.mework/lsp.json` or a built-in preset raises nothing. **Always allow** remembers the answer for that conversation, capped at the risk of the card you answered. An `operation` missing, empty or over 64 characters is refused before any path work.

## Behavior and limits

Answers are prose, not JSON: a location reads `path:line:character` relative to the workspace root, and a question nothing matches is answered with a sentence rather than an error. Hits in git-ignored files are dropped from definition, reference, implementation and workspace-symbol results when `git` is on `PATH`. A server starts the first time something asks about a file it claims, is shared by every conversation in that workspace, and stops when the app exits — so the first call in a project can be slow, or come back empty while it indexes. A file over 10 MB is not sent, and a request unanswered for 30 seconds fails with that as the reason.

## Related

- [grep](grep.html), [find](find.html), [read](read.html)
- [Code navigation](../lsp.html) — presets, `lsp.json` keys, diagnostics after an edit
- [Working with Mework](../working.html#tools-and-approvals)
