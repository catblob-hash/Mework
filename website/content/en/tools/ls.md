`ls` lists what is inside a directory of the workspace. The model reaches for it to orient itself in a tree it has not seen — which folders exist, what a package directory holds — before deciding what to `read`. A relative `path` is always resolved against the workspace, even when an approval has widened the scope past it.

## Approval

Calls are classified as reads. Inside the workspace, the app-data directory and any extra working directory granted to the conversation, a listing runs without asking at every security level. A path outside those roots is an outside read: `request_approval` raises an approval card, `allow_edits` and `full_access` let it through. **Always allow** can remember an outside listing for this tool in this conversation, capped at the risk level of the card you answered and forgotten when the app restarts. One directory is refused at every level and cannot be approved: `memory/` under the app-data directory, which only the long-term memory tools may open.

## Behavior and limits

The walk is breadth-first — every entry one level down, then every entry two levels down, and so on to `depth`, each directory in name order — never follows symlinks or junctions, and checks every entry against the authorized scope before it is shown or descended into. Entries are sorted, spelled with forward slashes, and directories carry a trailing `/`.

What stays unexpanded is decided the way Git would decide it. Inside a Git work tree, a directory `git ls-files` reports as ignored is listed with an `(ignored)` mark and not entered; outside one, dependency and build directories are treated the same way by name (`node_modules`, `vendor`, `target`, `build`, `dist`, `.venv`, `__pycache__` and a few more). Version-control directories such as `.git` are never entered. Ignored files are listed like any other file, and so is everything under a `path` that is itself ignored: name `target/debug` and you get `target/debug`. On a remote workspace the machine's own `git` decides.

The answer stops at 40,000 characters. Because the walk is breadth-first, what the limit cuts is the deepest level reached, and the last line says to which depth the listing is complete. An empty directory answers `(empty directory)`; a subdirectory that cannot be read adds a `[skipped]` line, while the listed directory failing to open fails the call.

## Related

- [find](find.html) — the same walk, filtered by a glob
- [grep](grep.html) — search file contents instead of names
- [read](read.html) — open one of the files you found
- [Working with Mework](../working.html#tools-and-approvals) — the security-level matrix
