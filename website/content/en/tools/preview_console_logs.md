Returns what the conversation's preview page printed to its own console — `console.debug/log/info/warn/error`, plus uncaught errors and unhandled promise rejections. The model reaches for it right after a reload or an interaction, to find out whether the page it just changed is throwing client-side. It reads the page, not the server: the dev server's stdout and stderr are `preview_logs`.

## Approval

Classified as a sensitive browser observation (`browser.sensitive_observation`, high risk, unbounded effect), because console text carries tokens and error context from whatever the page is signed in to. Manual, Accept edits and Plan mode all ask; Full access does not. **Always allow** never covers it: no `preview_*` page tool can carry a standing allowance, so every card is answered on its own and an allowance recorded elsewhere cannot answer one here. If you signed into the page yourself, the host asks separately for authorization to act on that tab — a confirmation no security level and no hook turns off.

## Behavior and limits

The page keeps a rolling buffer of the last 200 entries for its current document; the call returns the most recent `lines` of it, defaulting to 50 and clamped to 1–200. Each entry renders as `[level] text`, with the text capped at 8,000 characters; a footer naming both counts follows only when the buffer held more than were shown. `level: "warn"` keeps warnings and errors, `"error"` keeps errors alone, and an unrecognised value is refused rather than quietly read as `all`. Nothing left to show answers `No console logs.`

The call needs somewhere to look: a dev server running for this workspace, or a page the pane already holds. A sleeping page is woken for it; a crashed one is reset to `about:blank` first and says so. The call is bounded at 30 seconds.

## Decision-model parameters

Turn on its decision-model option in the conversation's preview tool window and it takes `query` and `threshold` as well: `level` filters as usual, and the entries left — the most recent `lines` of them, or all of them when `lines` is omitted — are each scored on their own by the TypeSafe Jev decision model, several to a request — an entry is never split, however many lines it spans — and every entry at or above `threshold` comes back whole, numbered by its place in that list. **Add decision-model parameters** keeps the plain listing beside it; **Decision model only** requires the pair on every call. The entries are sent to TypeSafe's API, which needs its key under Global settings → Decision model providers.

## Related

- [preview_logs](preview_logs.html) — the dev server's own output
- [preview_network](preview_network.html)
- [preview_eval](preview_eval.html)
- [Working with Mework](../working.html#the-built-in-browser)
- [preview_find_logs](preview_find_logs.html) — score this console and the server output together
