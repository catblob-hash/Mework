Answers an `alert`, `confirm` or `prompt` the preview page has opened. Dialogs are held natively — the page's JavaScript stays blocked inside the call, as in a real browser — and while one is held every other page tool is refused. The model reaches for it the moment a click it made pops a confirmation.

## Approval

The call is classified as an unbounded action (`tool.unbounded`, high risk), so `request_approval` and `allow_edits` ask first and `full_access` does not. **Always allow** is never offered for a page tool. If the held page is one you signed into yourself, the takeover card comes first and names the signed-in origin; no security level and no hook can answer that one on your behalf.

## Behavior and limits

`accept` defaults to true, so a call with no arguments accepts the dialog; false dismisses it. `prompt_text` is the answer typed into a `prompt`, is applied only when accepting, and is capped at 4,096 characters.

There has to be a dialog open. When there is not, the call fails and returns the last five dialogs the page raised, so the model can see whether it missed one.

Unlike most page tools, this one waits for the page to settle and then describes it: the dialog it answered, whether it was accepted, the prompt text, and a page block with the URL, title, loading state, console counts, the error lines logged during the action, and a bounded accessibility snapshot. If answering started a navigation, or the navigation policy refused one, the result says so. A page that opens a second dialog reports that modal state instead of the snapshot.

## Related

- [preview_click](preview_click.html) — the interaction that usually raises a dialog
- [preview_upload_image](preview_upload_image.html) — clears the other modal state, a file chooser
- [preview_snapshot](preview_snapshot.html) — the page once nothing is holding it
- [Working with Mework](../working.html#the-built-in-browser) — the built-in browser
