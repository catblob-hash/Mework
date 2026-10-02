Captures the conversation's preview page and hands the pixels back as an image the model can actually see, with the capture's width and height as the text beside it. It is what the built-in verify-after-editing workflow ends on: once a previewable change checks out, the model shares a capture rather than asking you to look for yourself. Anything that has to be measured goes to `preview_inspect`.

## Approval

Classified as a sensitive browser observation (`browser.sensitive_observation`, high risk, unbounded effect): the capture is the pixels of the current page, whatever it is signed in to. Manual and Accept edits ask; Full access does not. **Always allow** never remembers it — no `preview_*` page tool carries a standing allowance. A page you logged into yourself raises the separate takeover confirmation that no security level and no hook can turn off.

## Behavior and limits

Windows and macOS: the capture goes through `Page.captureScreenshot` on the page's Chromium — WebView2 on Windows, the embedded Chromium (CEF) on macOS — and on any other platform the tool answers with an error instead. The image is JPEG at quality 75, fitted to at most 800 device pixels wide and then multiplied by `scale` (0.1 to 1). A page never laid out is given a 1280×720 viewport for the capture, then has it taken back. The pointer overlay the agent draws is hidden first, and a page the pane is not showing is moved far offscreen, shown just long enough to composite, then hidden again where it was.

The pixels reach the model as a conversation image attachment, shrunk the way a picture you attach is: at most 2,000 px a side, then at most about 500 KB, with metadata stripped. The result reports the size the model actually receives; `preview_click` works from element uids, so nothing depends on the capture's own pixel size. It is advertised only to models that can see images. The call is bounded at 30 seconds.

## Related

- [preview_inspect](preview_inspect.html) — for anything that has to be measured
- [preview_snapshot](preview_snapshot.html)
- [Working with Mework](../working.html#the-built-in-browser)
