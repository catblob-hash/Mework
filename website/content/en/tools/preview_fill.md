Writes a value into one form control on the conversation's preview page. The model uses it to fill a field before submitting, to clear an input, or to choose an option in a `select`. The page's own listeners run, so a framework-controlled field keeps the value instead of reverting it.

## Approval

The call is classified as an unbounded action (`tool.unbounded`, high risk): `request_approval`, `allow_edits` and `plan` ask before it runs, `full_access` does not. No page tool is remembered by **Always allow**. If the page is one you signed into yourself, a takeover card comes first, naming the signed-in origin; that confirmation is not turned off by any security level and a hook cannot answer it.

## Behavior and limits

The target must be reachable: a `serverId` from this conversation's workspace, a running dev server, or a page the pane already holds.

One page-side script does the work. It focuses the element, then: for `select`, picks the option whose value or visible text equals `value`; for `input` and `textarea`, writes through the prototype's native `value` setter; for a `contenteditable` element, replaces its text. Anything else, a missing element, or an option that does not exist comes back as `Failed to fill element: <selector>`. On success a bubbling `input` and a bubbling `change` event follow.

The empty string is a legal value — that is how a field is cleared. `selector` is capped at 2,048 characters and may not be blank, `value` at 32,768 characters. The call gets 30 seconds, is refused while the page holds a dialog or file chooser, and answers with one line naming the selector rather than the state of the page.

## Decision-model parameters

Turn on its decision-model option in the conversation's preview tool window and `selector` can give way to `query`, a plain-language description of the input to fill: the host lists the page's elements (the same `[uid] role: "name"` lines as [preview_snapshot](preview_snapshot.html)), always adds a "none of the above" option after them, and has the TypeSafe Jev decision model choose. It is a choice, not a score, so there is no `threshold`. When the model names an element it is filled, and the result says which element, with the model's confidence and the runners-up; when it answers none of the above, nothing is filled, and the result reports the closest elements and then lists every element line the model was shown, so the next step needs no fresh snapshot. Turn on **Score each element on a miss** under the same option and, on a none-of-the-above, those lines are first scored against the description one by one — one request per line — and come back highest first with their scores. Under **Add decision-model parameters** a call gives `selector` or `query`, and one that gives both is refused; under **Decision model only** `selector` is no longer accepted and every call carries `query`. The element lines are sent to TypeSafe's API, which needs its key under Global settings → Decision model providers.

## Related

- [preview_click](preview_click.html) — press the button after filling the form
- [preview_inspect](preview_inspect.html) — read back what the field now holds
- [preview_upload_image](preview_upload_image.html) — the file-input equivalent
- [Working with Mework](../working.html#the-built-in-browser) — the built-in browser
