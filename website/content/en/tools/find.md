`find` locates files and directories by glob, anywhere under a directory of the workspace. The model reaches for it when it knows what something is called but not where it lives — every `*.test.ts`, a config file by name, the directory a package sits in — and then reads or greps what comes back.

## Approval

Calls are classified as reads. A search inside the workspace, the app-data directory or an extra working directory granted to the conversation runs without asking at every security level. A `path` outside those roots asks under `request_approval` and passes under `allow_edits` and `full_access`; **Always allow** can remember that for this tool in this conversation, capped at the risk level of the card you answered. `memory/` under the app-data directory is refused at every level, `full_access` included.

## Behavior and limits

Patterns use ordinary glob syntax — `?`, `*`, `**`, character classes and `{a,b}` alternates — and match case-sensitively. A `*` is not stopped by a path separator, and `**` is legal only as a whole path segment; spelled anywhere else it is a pattern error. Each entry is tested against its path relative to the searched directory and against its bare file name, and either hit counts. The search is fully recursive and follows no symlinks. Unlike `grep`, it hides nothing Git ignores, because a name lookup is often after exactly the ignored file — `.env`, a generated header. Matches in ignored paths (by Git's rules inside a work tree, under a dependency or build directory outside one) come after the others, marked `(ignored)`, with a line saying how many there are. Version-control directories match by name but are not walked into. Directories match too and are returned with a trailing `/`. Each group is sorted and spelled with forward slashes.

At most 100 matches come back, with a line naming the total when there are more; the walk examines at most 200,000 entries and says so when it stops early. Nothing matched answers `No matching files`.

## Related

- [ls](ls.html) — one directory, by depth rather than by pattern
- [grep](grep.html) — match file contents instead of names
- [read](read.html) — open what the search turned up
- [Working with Mework](../working.html#tools-and-approvals) — the security-level matrix
