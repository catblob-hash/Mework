Emulates a viewport size, and optionally a colour scheme, on the conversation's preview page. The model reaches for it to check a responsive layout at phone or tablet width, and to see a dark-mode rendering of a page whose media query it just wrote.

## Approval

A resize is classified as a local browser observation (`browser.local_observation`, low risk, read effect) — it adjusts the built-in browser rather than acting on the page, the same line `preview_snapshot` and `preview_inspect` get. No security level asks and no approval card appears; the **Reviewed** marker above is the tool picker's review flag, not an approval line. The one prompt it can still raise is the takeover confirmation a page you signed into yourself triggers before any page tool touches the tab; no security level and no hook can answer that card.

## Behavior and limits

Like every page tool it needs a running dev server or a page the pane already holds, and is bounded at 30 seconds.

Below 768 CSS pixels wide the emulation also makes the page believe it is a phone: device metrics carry `mobile: true` and a device pixel ratio of 2, the user agent becomes an Android Chrome string built from the WebView2 runtime's own Chromium major version, and touch emulation is enabled with five points and mouse-to-touch translation. Clearing the size takes all of that back.

A call that sets neither a size nor `colorScheme` is an error, as are an unknown `preset` or `colorScheme` and a custom size missing one dimension. A size stays on the tab, scaled down to fit when it is larger than the pane, until a `desktop` call clears it; `desktop` leaves `colorScheme` alone. The answer is one sentence per override applied.

## Related

- [preview_screenshot](preview_screenshot.html) — see the emulated layout
- [preview_inspect](preview_inspect.html) — measure an element at the new size
- [Working with Mework](../working.html#the-built-in-browser) — the built-in browser
