`find` locates files and directories by glob, anywhere under a directory of the workspace. The model reaches for it when it knows what something is called but not where it lives — every `*.test.ts`, a config file by name, the directory a package sits in — and then reads or greps what comes back.

## Approval

Calls are classified as reads. A search inside the workspace, the app-data directory or an extra working directory granted to the conversation runs without asking at every security level, Plan mode included. A `path` outside those roots asks under `request_approval` and Plan mode and passes under `allow_edits` and `full_access`; **Always allow** can remember that for this tool in this conversation, capped at the risk level of the card you answered. `memory/` under the app-data directory is refused at every level, `full_access` included.

## Behavior and limits

Patterns use ordinary glob syntax — `?`, `*`, `**`, character classes and `{a,b}` alternates — and match case-sensitively. A `*` is not stopped by a path separator, and `**` is legal only as a whole path segment; spelled anywhere else it is a pattern error. Each entry is tested against its path relative to the searched directory and against its bare file name, and either hit counts. The search is fully recursive, follows no symlinks and applies no ignore-file filtering, so `node_modules` and `.git` are walked unless you point `path` somewhere narrower. Directories match too and are returned with a trailing `/`. Results are sorted, spelled with forward slashes, and stop at 2,000 entries with a line saying the limit was reached; nothing matched answers `No matching files`.

## Related

- [ls](ls.html) — one directory, by depth rather than by pattern
- [grep](grep.html) — match file contents instead of names
- [read](read.html) — open what the search turned up
- [Working with Mework](../working.html#tools-and-approvals) — the security-level matrix
- [find_files](find_files.html) — when you know what a file does but not its name
