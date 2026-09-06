/**
 * Wheel scroll chaining that actually chains.
 *
 * Chromium LATCHES a wheel gesture to the scroller under the cursor when the
 * gesture begins. Once latched, reaching that scroller's end does not hand the
 * remaining delta to its parent: scrolling simply stops, and it stays stopped
 * until the gesture ends AND the pointer moves — which is why holding the mouse
 * still and keeping scrolling feels dead, while nudging the mouse frees it.
 *
 * `overscroll-behavior: auto` does not help. `auto` is the CSS default and only
 * says chaining is *permitted*; it does not defeat the latch. The only fix is to
 * carry the delta across the boundary ourselves.
 *
 * One listener at the window covers every scroll container in the app, so a new
 * scroller inherits the behavior without opting in.
 */

/** A line of text for `deltaMode: DOM_DELTA_LINE`, matching Chromium's own. */
const LINE_HEIGHT_PX = 16;

function isScrollable(element: Element, axis: "x" | "y"): boolean {
  const style = getComputedStyle(element);
  const overflow = axis === "y" ? style.overflowY : style.overflowX;
  if (overflow !== "auto" && overflow !== "scroll" && overflow !== "overlay") return false;
  const scrollSize = axis === "y" ? element.scrollHeight : element.scrollWidth;
  const clientSize = axis === "y" ? element.clientHeight : element.clientWidth;
  // A 1px slack: sub-pixel layout routinely leaves scrollHeight a hair above
  // clientHeight on boxes that are not actually scrollable.
  return scrollSize - clientSize > 1;
}

/** Whether `element` still has room to move `delta` along `axis`. */
function hasRoom(element: Element, axis: "x" | "y", delta: number): boolean {
  const position = axis === "y" ? element.scrollTop : element.scrollLeft;
  if (delta < 0) return position > 0;
  const scrollSize = axis === "y" ? element.scrollHeight : element.scrollWidth;
  const clientSize = axis === "y" ? element.clientHeight : element.clientWidth;
  // `scrollTop` is fractional under display scaling, so compare with the same
  // 1px slack used above rather than testing exact equality with the maximum.
  return scrollSize - clientSize - position > 1;
}

/**
 * Whether `element` refuses to pass leftover delta to its parent.
 *
 * A scroller that opts into `contain` or `none` wants to be a hard boundary —
 * that is a deliberate choice (a map, a nested editor), so chaining stops there
 * instead of being forced through.
 */
function trapsOverscroll(element: Element, axis: "x" | "y"): boolean {
  const style = getComputedStyle(element);
  const behavior = axis === "y" ? style.overscrollBehaviorY : style.overscrollBehaviorX;
  return behavior === "contain" || behavior === "none";
}

function pixelDelta(event: WheelEvent, raw: number): number {
  if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) return raw * LINE_HEIGHT_PX;
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) return raw * window.innerHeight;
  return raw;
}

function onWheel(event: WheelEvent) {
  // Someone already handled this deliberately, or it is a zoom gesture.
  if (event.defaultPrevented || event.ctrlKey) return;

  const axis: "x" | "y" = Math.abs(event.deltaY) >= Math.abs(event.deltaX) ? "y" : "x";
  const delta = pixelDelta(event, axis === "y" ? event.deltaY : event.deltaX);
  if (delta === 0) return;

  let node: Element | null = event.target instanceof Element
    ? event.target
    : null;
  // The scroller the browser has latched onto: the innermost scrollable
  // ancestor, whether or not it can still move.
  let latched: Element | null = null;

  while (node) {
    if (isScrollable(node, axis)) {
      if (hasRoom(node, axis, delta)) {
        // `latched` is still null only if this is the innermost scroller, which
        // is the one the browser latched onto — it has room, so the browser is
        // already scrolling it correctly and applying the delta here too would
        // scroll at double speed.
        if (latched === null) return;
        if (axis === "y") node.scrollTop += delta; else node.scrollLeft += delta;
        event.preventDefault();
        return;
      }
      if (latched === null) latched = node;
      // A scroller at its end that traps overscroll ends the chain.
      if (trapsOverscroll(node, axis)) return;
    }
    node = node.parentElement;
  }
}

/**
 * Installs the chaining handler. Idempotent, and returns a teardown so a test
 * can uninstall it.
 *
 * `passive: false` is required: the handler calls `preventDefault` to replace
 * the browser's stalled scroll with its own.
 */
export function startScrollChaining(target: Window = window): () => void {
  const handler = onWheel as EventListener;
  target.addEventListener("wheel", handler, { passive: false, capture: false });
  return () => target.removeEventListener("wheel", handler, { capture: false });
}
