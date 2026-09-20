Two shapes in one tool. Without `requestId` it lists the requests the preview page has made — id, method, url, status and failure reason. With one it reads that response's body back out of the browser cache — how the model checks an API payload it cannot see from the rendered page, when the page looks right but the data behind it does not.

## Approval

Classified as a sensitive browser observation (`browser.sensitive_observation`, high risk, unbounded effect): rows carry complete URLs, query parameters and response bodies from whatever the page is signed in to. Manual, Accept edits and Plan mode ask; Full access does not. **Always allow** never remembers it — no `preview_*` page tool carries a standing allowance. A page you logged into yourself raises a separate takeover confirmation no level or hook turns off.

## Behavior and limits

The ledger is filled from the browser's own network events and holds the page's last 500 rows, dropping the oldest first; only a page created from scratch starts it empty. A row reads `[requestId] METHOD url`, gains `→ status statusText` once the response arrives, and `[FAILED: reason]` when it did not. URLs are capped at 8,000 characters; a `data:` URL keeps its media type and its payload becomes a count of omitted characters. `filter: "failed"` keeps failures and every status of 400 or more; an unrecognised value is refused rather than quietly read as `all`. An empty listing answers `No network requests recorded.` or `No failed requests.`

A body that parses as JSON is pretty-printed, then capped at 10,000 characters with the raw total appended. A binary body is reported as base64 with its length and not shown. A body the cache has evicted is an error saying so: a `requestId` is only good while the response is cached. The call is bounded at 30 seconds.

## Related

- [preview_console_logs](preview_console_logs.html)
- [preview_logs](preview_logs.html) — the server's side
- [Working with Mework](../working.html#the-built-in-browser)
