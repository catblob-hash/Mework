`ls` lists what is inside a directory of the workspace. The model reaches for it to orient itself in a tree it has not seen — which folders exist, what a package directory holds — before deciding what to `read`. A relative `path` is always resolved against the workspace, even when an approval has widened the scope past it.

## Approval

Calls are classified as reads. Inside the workspace, the app-data directory and any extra working directory granted to the conversation, a listing runs without asking at every security level, Plan mode included. A path outside those roots is an outside read: `request_approval` and Plan mode raise an approval card, `allow_edits` and `full_access` let it through. **Always allow** can remember an outside listing for this tool in this conversation, capped at the risk level of the card you answered and forgotten when the app restarts. One directory is refused at every level and cannot be approved: `memory/` under the app-data directory, which only the long-term memory tools may open.

## Behavior and limits

The walk never follows symlinks or junctions, and every entry is checked against the authorized scope before it is shown or descended into. Entries are sorted, spelled with forward slashes, and directories carry a trailing `/`; the listing stops at 2,000 entries and says that the limit was reached. An empty directory answers `(empty directory)`, and a directory that cannot be traversed fails the whole call. Nothing is filtered by `.gitignore`: `.git`, `node_modules` and build output are walked like anything else, so narrow `path` or reach for `find` instead. Output over 64 KiB is truncated.

## Related

- [find](find.html) — the same walk, filtered by a glob
- [grep](grep.html) — search file contents instead of names
- [read](read.html) — open one of the files you found
- [Working with Mework](../working.html#tools-and-approvals) — the security-level matrix
- [find_files](find_files.html) — let the decision model pick paths out of a tree too large to read
