Reads the preview page's accessibility tree and returns it as an indented outline — one line per node carrying its role, its accessible name and, where it has one, its value. It is the model's cheapest reliable view of the page's exact text and structure. The model reaches for it after a navigation or an interaction, to confirm the page says what it should.

## Approval

Classified as a local browser observation (`browser.local_observation`, low risk, read effect), scoped to the workspace and the application's own data directory, so no security level asks for it; the **Reviewed** marker above is the catalog's review flag, not the approval line. A `PreToolUse` hook can still require a confirmation, and the takeover card the host raises for a page you logged into yourself is answered by no level, hook or standing allowance.

## Behavior and limits

The accessibility domain is enabled only for the read and released again. Lines read `[uid] role: "name" (value: "…")`, indented by depth, with names and values capped at 200 characters each. A presentational or generic wrapper with nothing interesting below it is printed through, a generic wrapper holding a single child is collapsed into it, an SVG root's internals and a decorative image's children are dropped, and past depth 8 a node's children give way to `... (N descendants)`. The whole snapshot is capped at 12,000 characters and closes with a line naming the real total. A page with nothing to report answers `No accessible content found.`

The `uid` on each line comes from a counter that runs for the life of the page, so a later snapshot never reuses an earlier one. It is not an address: `preview_click` and `preview_fill` take CSS selectors. The call needs a dev server running for this workspace or a page the pane already holds, and is bounded at 30 seconds.

## Decision-model parameters

Turn on its decision-model option in the conversation's preview tool window and it takes `query` and `threshold` as well: the page's element lines are cut into runs the TypeSafe Jev decision model scores, and instead of the whole snapshot only the elements scoring at or above `threshold` come back, each with a CSS selector to hand straight to [preview_click](preview_click.html), [preview_fill](preview_fill.html) or [preview_inspect](preview_inspect.html). **Add decision-model parameters** keeps the whole snapshot beside it; **Decision model only** requires the pair on every call. The element lines are sent to TypeSafe's API, which needs its key under Global settings → Decision model providers.

## Related

- [preview_inspect](preview_inspect.html)
- [preview_screenshot](preview_screenshot.html)
- [Working with Mework](../working.html#the-built-in-browser)
