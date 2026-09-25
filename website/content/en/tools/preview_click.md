Clicks one element on the conversation's preview page. The model reaches for it after a snapshot has told it what is on the page: submit the form, open the menu, follow the flow it just changed. Mouse input is dispatched by the host through the debugging protocol, not injected as page script.

## Approval

The call is classified as an unbounded action (`tool.unbounded`, high risk), so `request_approval`, `allow_edits` and `plan` ask before it runs and `full_access` does not. **Always allow** is never offered for a page tool, so the next click asks again. Underneath that, a page you signed into yourself raises a takeover card first, naming the signed-in origin; no security level and no hook can answer that one for you, and the grant lapses as soon as the page leaves that origin.

## Behavior and limits

It needs something to act on: a `serverId` from this workspace, a dev server `preview_start` has running, or a page the pane already holds.

The element is found with `document.querySelector`, scrolled to the centre of the viewport and clicked with a press/release pair at the centre of its bounding rectangle — twice when `doubleClick` is set. A selector that matches nothing, or an element with no rectangle, comes back as `Failed to click element: <selector>`. `selector` is capped at 2,048 characters and may not be blank.

The call gets 30 seconds. A dialog or file chooser the page is already holding refuses the click and names the tool that clears it; one the click itself opens blocks the page mid-action, and the result becomes `{"interrupted": "modal-state"}`. Success is one line naming the selector, not a description of the page afterwards.

## Related

- [preview_fill](preview_fill.html) — type into an input instead of clicking it
- [preview_snapshot](preview_snapshot.html) — what is on the page, before and after
- [preview_dialog](preview_dialog.html) — answer a dialog a click opened
- [Working with Mework](../working.html#the-built-in-browser) — the browser and `.mework/launch.json`
