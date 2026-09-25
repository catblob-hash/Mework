Resolves one CSS selector against the preview page and returns a JSON record of the element it found: tag name, text, class and id, computed styles, bounding box, and the React component behind it when there is one. This is what the model uses to verify colours, fonts, spacing and dimensions, since a screenshot cannot be measured.

## Approval

Classified as a local browser observation (`browser.local_observation`, low risk, read effect), scoped to the workspace and the application's own data directory, so no security level asks for it; the **Reviewed** marker above is the catalog's review flag, not the approval line. The classifier does refuse the call outright when `selector` is missing, is not a string, or runs past 2,048 characters, and it does so before a page is created for it. A `PreToolUse` hook can still require a confirmation, and the takeover card for a page you logged into yourself is answered by no level, hook or standing allowance.

## Behavior and limits

Only the first match is inspected. No match is not an error: the result is the plain text `Element not found:` followed by the selector. With no `styles` given, the record carries ten computed properties — `color`, `background-color`, `font-size`, `font-weight`, `padding`, `margin`, `width`, `height`, `display` and `visibility`. You may ask for up to 64 instead; a property the computed style does not carry is simply absent from the record rather than reported empty.

`text` is the element's `innerText` truncated to 500 characters and `className` to 200; `value` appears only when the element carries a non-empty `value` attribute. `boundingBox` is the content box as `x`, `y`, `width` and `height`. When the element sits inside a React tree, `reactComponent` and `reactProps` are added from its fiber. The DOM and CSS domains are released on every exit path. The call is bounded at 30 seconds.

## Related

- [preview_snapshot](preview_snapshot.html) — to find the selector worth inspecting
- [preview_screenshot](preview_screenshot.html)
- [Working with Mework](../working.html#the-built-in-browser)
