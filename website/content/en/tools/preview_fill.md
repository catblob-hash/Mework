Writes a value into one form control on the conversation's preview page. The model uses it to fill a field before submitting, to clear an input, or to choose an option in a `select`. The page's own listeners run, so a framework-controlled field keeps the value instead of reverting it.

## Approval

The call is classified as an unbounded action (`tool.unbounded`, high risk): `request_approval`, `allow_edits` and `plan` ask before it runs, `full_access` does not. No page tool is remembered by **Always allow**. If the page is one you signed into yourself, a takeover card comes first, naming the signed-in origin; that confirmation is not turned off by any security level and a hook cannot answer it.

## Behavior and limits

The target must be reachable: a `serverId` from this conversation's workspace, a running dev server, or a page the pane already holds.

One page-side script does the work. It focuses the element, then: for `select`, picks the option whose value or visible text equals `value`; for `input` and `textarea`, writes through the prototype's native `value` setter; for a `contenteditable` element, replaces its text. Anything else, a missing element, or an option that does not exist comes back as `Failed to fill element: <selector>`. On success a bubbling `input` and a bubbling `change` event follow.

The empty string is a legal value — that is how a field is cleared. `selector` is capped at 2,048 characters and may not be blank, `value` at 32,768 characters. The call gets 30 seconds, is refused while the page holds a dialog or file chooser, and answers with one line naming the selector rather than the state of the page.

## Related

- [preview_click](preview_click.html) — press the button after filling the form
- [preview_inspect](preview_inspect.html) — read back what the field now holds
- [preview_upload_image](preview_upload_image.html) — the file-input equivalent
- [Working with Mework](../working.html#the-built-in-browser) — the built-in browser
