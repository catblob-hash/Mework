Puts an image that is already in the conversation into a file input on the preview page. The model reaches for it when a page it is testing needs a real upload, and it is also the only action that clears a file chooser the page has opened.

## Approval

The call sends local data to the page (`browser.image_upload`, high risk), so it asks at `request_approval`, `allow_edits` and `plan`; only `full_access` clears that line. No page tool is remembered by **Always allow**, and on a page you signed into yourself the takeover confirmation comes first, which no security level or hook can answer for you. There is no manual route: the image number only means something inside a live run, so executing or approving the tool outside one is refused outright.

## Behavior and limits

`image_id` is resolved against the transcript: images you attached and images produced by tools, memory tools excluded. The host writes those bytes into a single-use temporary directory, hands the page the real path, and deletes it afterwards whether or not the upload succeeded. The model never supplies a filesystem path.

The page-visible name comes from `filename` or the attachment, with characters Windows forbids replaced, an extension added from the image's type when none is given, and a 160-byte cap. A hidden input works; visibility is not required. With no file chooser open the call must name a `selector`; there is no fallback to the first file input on the page.

The result reports the element, the file count, the image number and name, and the page once it settles. Only models that can see images are offered it; the call is bounded at 30 seconds.

## Related

- [preview_fill](preview_fill.html) — the other way into a form
- [preview_click](preview_click.html) — the click that opens the chooser
- [preview_dialog](preview_dialog.html) — the other modal state a page can hold
- [Working with Mework](../working.html#images) — image attachments and `[Image #N]`
