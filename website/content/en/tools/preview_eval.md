Evaluates one JavaScript expression in the preview page and hands the result back to the model. It is the inspection tool of last resort: reading state a snapshot does not expose, querying the DOM, or moving the page with `window.location` — this surface has no navigate tool.

## Approval

The call is classified as an unbounded action (`tool.unbounded`, high risk), so `request_approval` and `allow_edits` raise an approval card before it runs, and `full_access` does not. **Always allow** is never offered for a page tool. On a page you signed into yourself, the run loop first asks whether the Agent may take the tab over, naming the signed-in origin; that card is not disabled by any security level and a hook cannot answer it.

## Behavior and limits

The expression is evaluated in the page's own context with the result returned by value; a promise is awaited first. The completion value comes back as pretty-printed JSON, JavaScript `undefined` comes back as the text `undefined`, and a thrown exception becomes the call's error message with the exception's own description.

`expression` may not be blank and is capped at 65,536 characters. The evaluation gets 15 seconds and the whole call 30 seconds.

A navigation the script starts is still judged by the page's navigation policy at the conversation's security level; a refused one is rolled back and leaves the page where it was. While a dialog or file chooser is held, evaluation is refused until the tool that clears it runs, and a dialog that opens during the evaluation ends it with `{"interrupted": "modal-state"}`. It is refused too while you hold control of the pane.

## Related

- [preview_snapshot](preview_snapshot.html) — structured page content without a script
- [preview_inspect](preview_inspect.html) — computed styles and geometry for one selector
- [preview_console_logs](preview_console_logs.html) — what the page logged
- [Working with Mework](../working.html#the-built-in-browser) — one page per conversation
