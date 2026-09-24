`find_files` picks out the paths under a directory that a plain-language description points at, without the model reading a listing: the directory is walked, the paths are cut into groups the TypeSafe Jev decision model scores against the query, and the paths of every group that clears `threshold` are scored one by one, so what comes back is a handful of paths with their scores. The model reaches for it when it knows what a file is for but not what it is called — the code that stores credentials, the page that renders sign-in — where `find` needs a name to glob for.

## Approval

Calls are classified as reads, judged by the same path rules as `ls` and `find`. A directory inside the workspace, the app-data directory or an extra working directory granted to the conversation is searched without asking at every security level, Plan mode included. A `path` outside those roots asks under `request_approval` and Plan mode and passes under `allow_edits` and `full_access`; **Always allow** can remember that for this tool in this conversation, capped at the risk level of the card you answered. `memory/` under the app-data directory is refused at every level, `full_access` included, and never appears in a listing. Sending the paths to TypeSafe is not a separate approval: enabling the tool and saving the key is the opt-in.

## Behavior and limits

`path` defaults to the workspace root and must be a directory. `depth` counts levels below it the way `ls` does — 0 is its own entries only — and defaults to 6 rather than `ls`'s 1; more than 8 is refused. The walk lists files and directories alike, directories with a trailing `/`, follows no symlinks, checks every entry against the authorized scope before listing it or descending into it, and applies no ignore-file filtering: `.git`, `node_modules` and build output are listed and scored like anything else, so narrow `path` or `depth`. Paths are relative to the workspace, with forward slashes, ready for `read`. The walk stops at 20,000 entries and the answer says the listing was cut; on this machine a directory that cannot be traversed fails the call. An empty directory answers `<path> has no entries to score.` without a request. `query` is trimmed and capped at 2,000 characters; `threshold` must lie between 0 and 1 and is rounded to three decimals.

Every path of the sorted listing is a block of its own, scored on its own, and many share one request — as many as the request budget holds, and never more than 32. Requests run eight at a time under the pace every TypeSafe request Mework makes shares — 32 at once after a quiet spell, then 18 a second, under TypeSafe's 1,200 a minute — so the full 20,000 entries are 625 requests and about 35 seconds rather than a coarser pass.

The answer opens with a count — `1 hit at or above 0.600 for query "the sign-in page" (8 entries under . scored, 1 request).` — then one line per path that cleared, highest score first, such as `src/auth/login_screen.tsx (score 1.000)`. Scores carry the threshold's three decimals, so a reported score can be passed back as a threshold. When nothing clears, the answer says so and names the three best paths with their scores. Output over 64 KiB is truncated.

Answers of `429` and `529` are retried twice, after 1 s and 2 s or the server's `Retry-After` up to 10 s, and each attempt has 30 s. A request that still fails leaves its blocks unscored, and the answer counts the failed requests and the blocks they left unscored; the call fails only when nothing at all was scored. A missing key, or one TypeSafe rejects, fails the call with a message pointing to the key setting. Stopping the run returns at once with `nothing was returned`; requests already in flight finish on their own. On a WSL or SSH workspace the walk runs on that machine with the same depth and entry cap, and only the paths come back.

## The decision model

`find_files` is its own row under **Files and search** in the tool picker, marked **Decision model**, not in the **File tools** window behind the `files` row. Seeded presets leave it off, because it cannot run until TypeSafe's key is saved under Global settings → Decision model providers, which keeps it in the operating system's credential store. Each request sends TypeSafe's API the query and the paths it scores; file contents are never read or sent. A score is Jev's probability-weighted answer on four levels — unrelated, loosely related, relevant (contains part of what was asked), direct hit — mapped onto 0 to 1, so the levels sit at 0, 0.333, 0.667 and 1. Subagents are not barred from it.

## Related

- [find](find.html) — when you know the name, by glob
- [ls](ls.html) — read the tree yourself, level by level
- [find_content](find_content.html) — then find the place inside a file
- [Decision-model tools](../tools.html#decision-model) — what these tools have in common
- [Working with Mework](../working.html#tools-and-approvals) — the security-level matrix
