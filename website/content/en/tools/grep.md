`grep` searches text files line by line with a regular expression and returns the hits as `path:line:content`. The model reaches for it to find where a symbol, string or TODO actually lives before opening anything, and to check that a rename left nothing behind. `path` may be a single file or a directory that is searched recursively.

## Approval

Calls are classified as reads. A search that stays inside the workspace, the app-data directory or an extra working directory granted to the conversation runs without asking at every security level. A path outside those roots asks under `request_approval`, and passes under `allow_edits` and `full_access`; **Always allow** can remember that for this tool in this conversation, capped at the risk level of the card you answered. `memory/` under the app-data directory is refused at every level, `full_access` included.

## Behavior and limits

Patterns use Rust `regex` syntax — character classes, alternation, anchors and repetition, but no backreferences or lookaround — and every line is matched on its own, so `^` and `$` bind to line ends.

Inside a Git work tree the search covers what Git would show: tracked files and untracked files that are not ignored, so build output, `node_modules` and whatever else `.gitignore` names produce no hits. Outside a work tree, dependency and build directories are skipped by name, as [ls](ls.html) describes. Version-control directories are never searched. A `path` inside an ignored directory is searched in full, and a single file named as `path` is searched whatever the rules say. Symlinks are not followed. Files larger than 2 MiB are skipped, and so is any file with a NUL byte among its first 8,192 bytes, which is how binaries are recognised; invalid UTF-8 elsewhere is replaced rather than fatal.

Line numbers are 1-based and each reported line is cut at 500 characters. A call returns 250 matching lines unless `limit` asks for another number, up to 1,000; when more follow, a last line gives the `offset` of the next page, and `offset` can reach 100,000. An entry that could not be read adds a `[skipped]` line (the first 20 are named, the rest counted) rather than failing the call, and no hits at all answers `No matches found`. A result longer than 20,000 characters is saved to a file under the app-data directory and replaced by its path and first 2,000 characters; `read` or `grep` that file for the rest.

## Related

- [find](find.html) — match names instead of contents
- [read](read.html) — open a file at the line a match reported
- [lsp](lsp.html) — definitions and references instead of text matches
- [bash](bash.html) — `rg` when you need flags this tool does not expose
