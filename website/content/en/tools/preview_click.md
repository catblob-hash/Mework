Clicks one element on the conversation's preview page. The model reaches for it after a snapshot has told it what is on the page: submit the form, open the menu, follow the flow it just changed. Mouse input is dispatched by the host through the debugging protocol, not injected as page script.

## Approval

The call is classified as an unbounded action (`tool.unbounded`, high risk), so `request_approval`, `allow_edits` and `plan` ask before it runs and `full_access` does not. **Always allow** is never offered for a page tool, so the next click asks again. Underneath that, a page you signed into yourself raises a takeover card first, naming the signed-in origin; no security level and no hook can answer that one for you, and the grant lapses as soon as the page leaves that origin.

## Behavior and limits

It needs something to act on: a `serverId` from this workspace, a dev server `preview_start` has running, or a page the pane already holds.

The element is found with `document.querySelector`, scrolled to the centre of the viewport and clicked with a press/release pair at the centre of its bounding rectangle — twice when `doubleClick` is set. A selector that matches nothing, or an element with no rectangle, comes back as `Failed to click element: <selector>`. `selector` is capped at 2,048 characters and may not be blank.

The call gets 30 seconds. A dialog or file chooser the page is already holding refuses the click and names the tool that clears it; one the click itself opens blocks the page mid-action, and the result becomes `{"interrupted": "modal-state"}`. Success is one line naming the selector, not a description of the page afterwards.

## Decision-model parameters

Turn on its decision-model option in the conversation's preview tool window and `selector` can give way to `query`, a plain-language description of the element to click: the host lists the page's elements (the same `[uid] role: "name"` lines as [preview_snapshot](preview_snapshot.html)), always adds a "none of the above" option after them, and has the TypeSafe Jev decision model choose. It is a choice, not a score, so there is no `threshold`. When the model names an element it is clicked, and the result says which element, with the model's confidence and the runners-up; when it answers none of the above, nothing is clicked, and the result reports the closest elements and then lists every element line the model was shown, so the next step needs no fresh snapshot. Turn on **Score each element on a miss** under the same option and, on a none-of-the-above, those lines are first scored against the description one by one — one request per line — and come back highest first with their scores. Under **Add decision-model parameters** a call gives `selector` or `query`, and one that gives both is refused; under **Decision model only** `selector` is no longer accepted and every call carries `query`. The element lines are sent to TypeSafe's API, which needs its key under Global settings → Decision model providers.

## Related

- [preview_fill](preview_fill.html) — type into an input instead of clicking it
- [preview_snapshot](preview_snapshot.html) — what is on the page, before and after
- [preview_dialog](preview_dialog.html) — answer a dialog a click opened
- [Working with Mework](../working.html#the-built-in-browser) — the browser and `.mework/launch.json`
