`web_search` puts one query to the search backend this conversation named and returns what comes back as the tool's own result. The model reaches for it when an answer is neither in the workspace nor in its own knowledge. Several calls in one turn overlap, and all of them settle before the round ends.

## Approval

A search is classified as an unbounded action with no path (rule `web.search`, high risk). `request_approval` and `allow_edits` ask; `full_access` does not. One card authorizes the entire call: the query, every page the backend opens for it, and everything that comes back. The prompt is not mandatory, so **Always allow** can remember it for that conversation. The query is checked before any card appears: 2 to 200 characters.

## Behavior and limits

The tool is offered whenever web access is on and a search backend is named — even one that no longer resolves, so a misconfiguration is a repairable error instead of a missing tool. **Native** delegates to the conversation's own provider and model in an isolated request carrying the provider's server-side search tool and no client tools; the result is that model's written report plus the sites the provider retrieved, and the per-call search budget applies only here. A **catalog provider** is called by the host and returns normalized title, URL and content: capped at the conversation's result count (default 5, ceiling 50), filtered by whichever domain list is in effect, then trimmed to a whole-call token budget split evenly across results (default 2,000, ceiling 200,000; a trimmed body ends in `...`). Those entries each carry an `id` for citations, and both envelopes are marked `untrustedWebContent`. The call mints no task identity, so `task_wait` cannot name it. The first run that offers the tool pins the backend for the rest of the conversation.

## Related

- [web_fetch](web_fetch.html)
- [Working with Mework](../working.html#web-search-and-fetch)
- [Prompt profiles](../prompt-profiles.html)
