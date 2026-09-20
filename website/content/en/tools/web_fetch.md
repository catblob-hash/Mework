`web_fetch` retrieves the readable text of pages the model already has URLs for, up to twenty in one call. The model reaches for it after a search has produced links, or when you paste a URL and ask what is on it. Pages are retrieved by the host or by the conversation's own upstream, never by the browser pane.

## Approval

A fetch is classified as an unbounded action with no path (rule `web.fetch`, high risk), the same boundary `web_search` uses. `request_approval`, `allow_edits` and Plan mode ask; `full_access` does not. One card authorizes the whole list, so read the URLs before answering. The prompt is not mandatory, so **Always allow** can remember it for that conversation. The list is validated before any card is shown: one to twenty absolute `http(s)` URLs, each non-blank and under 2,048 characters; anything else is refused outright.

## Behavior and limits

The tool appears only when a fetch backend resolves, so a conversation whose upstream keeps retrieval inside its search tool sees `web_search` alone. **Native** fetching exists on the Anthropic and Bedrock families, which return the page as plain text. A **catalog provider** — Jina, Firecrawl, Querit, or the direct `fetch` provider — is called by the host instead, and only that path applies the conversation's domain filter and token budget. The direct provider refuses a URL carrying a username or password, follows at most five redirects, re-checking every hop, caps a page at 4 MiB and 200,000 characters after extraction, and times out at 30 s (15 s to connect); the connection is pinned to the address it checked, so DNS cannot be swapped underneath it. A loopback, private or link-local target is refused unless the conversation is at `full_access` and the model named it directly; redirects into one are refused at every level. Results arrive sanitized in an `untrustedWebContent` envelope, one `id` per entry.

## Related

- [web_search](web_search.html)
- [Working with Mework](../working.html#web-search-and-fetch)
- [Prompt profiles](../prompt-profiles.html)
