/**
 * Wheel scroll chaining that actually chains, and keeps moving smoothly when it
 * does.
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
 * Carrying it is only half the job. A wheel tick the browser handles is not a
 * jump: Chromium animates the scroller to its new offset over 100-200ms. A
 * chained tick written straight into `scrollTop` teleports instead, so the
 * gesture visibly changes character at the moment it crosses the boundary —
 * smooth inside the child, stepped in the parent, with the pointer never having
 * moved. The chained delta therefore drives the same animation Chromium would
 * have run: the same curve, the same duration, and the same retargeting when
 * the next tick arrives mid-flight.
 *
 * That animation is deliberately not gated on `prefers-reduced-motion`. It adds
 * no page motion; it preserves the platform's own scroll physics across a
 * boundary, and Chromium does not drop those under a motion reduction either.
 * Gating it would put back exactly the jump this code exists to remove.
 *
 * One listener at the window covers every scroll container in the app, so a new
 * scroller inherits the behavior without opting in.
 */

type Axis = "x" | "y";

/** A line of text for `deltaMode: DOM_DELTA_LINE`, matching Chromium's own. */
const LINE_HEIGHT_PX = 16;

/**
 * How long Chromium takes to animate one wheel tick, reproduced from `cc`'s
 * inverse-delta curve: a small tick gets the full 200ms, and the duration ramps
 * down to 100ms by 480px so a fast flick does not trail behind the wheel.
 */
const RAMP_START_PX = 120;
const RAMP_END_PX = 480;
const LONGEST_MS = 200;
const SHORTEST_MS = 100;

function durationFor(distance: number): number {
  const travel = Math.abs(distance);
  if (travel <= RAMP_START_PX) return LONGEST_MS;
  if (travel >= RAMP_END_PX) return SHORTEST_MS;
  const ramp = (travel - RAMP_START_PX) / (RAMP_END_PX - RAMP_START_PX);
  return LONGEST_MS + ramp * (SHORTEST_MS - LONGEST_MS);
}

/** A cubic Bézier timing function; the endpoints are implicitly (0,0) and (1,1). */
type Curve = { x1: number; y1: number; x2: number; y2: number };

/** One axis of the curve at parameter `t`. */
function axisAt(p1: number, p2: number, t: number): number {
  const c = 3 * p1;
  const b = 3 * (p2 - p1) - c;
  const a = 1 - c - b;
  return ((a * t + b) * t + c) * t;
}

/** d/dt of `axisAt`. */
function axisSlope(p1: number, p2: number, t: number): number {
  const c = 3 * p1;
  const b = 3 * (p2 - p1) - c;
  const a = 1 - c - b;
  return (3 * a * t + 2 * b) * t + c;
}

/**
 * The parameter at which the curve's x reaches `x`. Newton-Raphson from a
 * linear guess settles in two or three steps for the curves used here;
 * bisection picks up the rare start that sends Newton off the interval.
 */
function parameterAt(curve: Curve, x: number): number {
  let t = x;
  for (let step = 0; step < 8; step += 1) {
    const error = axisAt(curve.x1, curve.x2, t) - x;
    if (Math.abs(error) < 1e-5 && t >= 0 && t <= 1) return t;
    const slope = axisSlope(curve.x1, curve.x2, t);
    if (Math.abs(slope) < 1e-6) break;
    t -= error / slope;
  }
  let low = 0;
  let high = 1;
  t = x;
  for (let step = 0; step < 32; step += 1) {
    const value = axisAt(curve.x1, curve.x2, t);
    if (Math.abs(value - x) < 1e-5) break;
    if (value < x) low = t; else high = t;
    t = (low + high) / 2;
  }
  return t;
}

/** The fraction of the distance travelled once `progress` of the time has passed. */
function easedAt(curve: Curve, progress: number): number {
  if (progress <= 0) return 0;
  if (progress >= 1) return 1;
  return axisAt(curve.y1, curve.y2, parameterAt(curve, progress));
}

/** dy/dx at `progress`: distance covered per unit of time, both normalised. */
function velocityAt(curve: Curve, progress: number): number {
  const t = parameterAt(curve, Math.min(1, Math.max(0, progress)));
  const horizontal = axisSlope(curve.x1, curve.x2, t);
  if (Math.abs(horizontal) < 1e-6) return 0;
  return axisSlope(curve.y1, curve.y2, t) / horizontal;
}

/**
 * Chromium's ease-in-out (0.42, 0, 0.58, 1), with the first control point
 * rotated so the curve leaves the origin at `slope` instead of flat.
 *
 * Retargeting mid-flight onto a plain ease-in-out would drop the speed back to
 * zero on every wheel tick, and a wheel held down delivers a tick every frame
 * or two — the result stutters instead of gliding. Rotating the control point
 * is how `cc` carries the current speed into the new segment.
 */
function easeInOut(slope = 0): Curve {
  const bounded = Math.max(-1000, Math.min(1000, slope));
  const x1 = Math.sqrt(0.42 * 0.42 / (bounded * bounded + 1));
  return { x1, y1: bounded * x1, x2: 0.58, y2: 1 };
}

function offsetOf(element: Element, axis: Axis): number {
  return axis === "y" ? element.scrollTop : element.scrollLeft;
}

function moveTo(element: Element, axis: Axis, offset: number): void {
  if (axis === "y") element.scrollTop = offset; else element.scrollLeft = offset;
}

/** The largest offset `element` can hold on `axis`. */
function furthestOffset(element: Element, axis: Axis): number {
  const scrollSize = axis === "y" ? element.scrollHeight : element.scrollWidth;
  const clientSize = axis === "y" ? element.clientHeight : element.clientWidth;
  return Math.max(0, scrollSize - clientSize);
}

/** A chained scroll in flight. */
type Glide = {
  element: Element;
  axis: Axis;
  from: number;
  target: number;
  startedAt: number;
  durationMs: number;
  curve: Curve;
  /** When this glide last painted, which is the moment `from` and `wrote` describe. */
  lastFrame: number;
  /** The last offset written here, which tells this module's motion from anyone else's. */
  wrote: number;
  frame: number;
};

/**
 * One glide per scroller, not one in total. A tick that fills the child and
 * spills into the parent leaves both moving, exactly as the browser leaves both
 * moving when it animates them itself; keeping a single slot would strand the
 * child wherever its animation had got to.
 */
const glides = new Map<Element, Glide>();

function isGliding(element: Element, axis: Axis): boolean {
  const glide = glides.get(element);
  return glide !== undefined && glide.axis === axis;
}

function cancelGlide(element: Element): void {
  const glide = glides.get(element);
  if (!glide) return;
  if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(glide.frame);
  glides.delete(element);
}

function cancelEveryGlide(): void {
  for (const element of [...glides.keys()]) cancelGlide(element);
}

/** Where a scroller is headed, which is where it already is unless it is gliding. */
function settledOffset(element: Element, axis: Axis): number {
  const glide = glides.get(element);
  return glide && glide.axis === axis ? glide.target : offsetOf(element, axis);
}

function isScrollable(element: Element, axis: Axis): boolean {
  const style = getComputedStyle(element);
  const overflow = axis === "y" ? style.overflowY : style.overflowX;
  if (overflow !== "auto" && overflow !== "scroll" && overflow !== "overlay") return false;
  const scrollSize = axis === "y" ? element.scrollHeight : element.scrollWidth;
  const clientSize = axis === "y" ? element.clientHeight : element.clientWidth;
  // A 1px slack: sub-pixel layout routinely leaves scrollHeight a hair above
  // clientHeight on boxes that are not actually scrollable.
  return scrollSize - clientSize > 1;
}

/**
 * Whether `element` still has room to move `delta` along `axis`.
 *
 * A glide in flight counts as already spent: the room left is measured from
 * where the scroller is going, not from where it has got to, so a tick that
 * arrives mid-glide chains onward once the glide is already aimed at the end
 * instead of piling into a target that cannot move any further.
 */
function hasRoom(element: Element, axis: Axis, delta: number): boolean {
  const position = settledOffset(element, axis);
  if (delta < 0) return position > 0;
  // `scrollTop` is fractional under display scaling, so compare with the same
  // 1px slack used above rather than testing exact equality with the maximum.
  return furthestOffset(element, axis) - position > 1;
}

/**
 * Whether `element` refuses to pass leftover delta to its parent.
 *
 * A scroller that opts into `contain` or `none` wants to be a hard boundary —
 * that is a deliberate choice (a map, a nested editor), so chaining stops there
 * instead of being forced through.
 */
function trapsOverscroll(element: Element, axis: Axis): boolean {
  const style = getComputedStyle(element);
  const behavior = axis === "y" ? style.overscrollBehaviorY : style.overscrollBehaviorX;
  return behavior === "contain" || behavior === "none";
}

function pixelDelta(event: WheelEvent, raw: number): number {
  if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) return raw * LINE_HEIGHT_PX;
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) return raw * window.innerHeight;
  return raw;
}

function advance(glide: Glide, time: number): void {
  // A newer glide has taken this scroller over; this one is a stale frame.
  if (glides.get(glide.element) !== glide) return;
  // Somebody else moved this scroller — a native scroll once the pointer
  // crossed into it, a `scrollIntoView`, a pane sticking itself to the bottom.
  // They win: two writers per frame is what jitter is made of.
  if (Math.abs(offsetOf(glide.element, glide.axis) - glide.wrote) > 1) {
    glides.delete(glide.element);
    return;
  }
  const progress = Math.min(1, (time - glide.startedAt) / glide.durationMs);
  moveTo(
    glide.element,
    glide.axis,
    glide.from + (glide.target - glide.from) * easedAt(glide.curve, progress)
  );
  // Read back rather than trusting the write: the browser clamps and snaps, and
  // the comparison above is only meaningful against what actually landed.
  glide.wrote = offsetOf(glide.element, glide.axis);
  glide.lastFrame = time;
  if (progress >= 1) {
    glides.delete(glide.element);
    return;
  }
  glide.frame = requestAnimationFrame((next) => advance(glide, next));
}

/** Moves `element` by `delta`, animated the way the browser animates a wheel tick. */
function glideBy(element: Element, axis: Axis, delta: number): void {
  const continuing = isGliding(element, axis) ? glides.get(element) : undefined;
  const from = offsetOf(element, axis);
  const target = Math.max(0, Math.min(furthestOffset(element, axis), settledOffset(element, axis) + delta));
  const distance = target - from;
  // A retargeted segment is anchored to the frame the glide last painted, not
  // to this instant. `requestAnimationFrame` reports the time the frame began,
  // and a wheel event is dispatched inside that same frame — so a segment
  // stamped "now" is still at progress zero when the very next callback runs,
  // and the scroller stands still for one frame at every tick. Anchoring to the
  // last painted frame is also what `from` already describes: that frame is
  // where the scroller currently is.
  const startedAt = continuing
    ? continuing.lastFrame
    : (typeof performance !== "undefined" ? performance.now() : 0);

  // Nothing left to travel: the scroller is pinned against its end, or a
  // reversing tick has cancelled out the glide already in flight. Also the
  // only path when there are no frames to animate on (jsdom without a visual
  // loop), where landing on the target beats refusing to scroll.
  if (Math.abs(distance) < 1 || typeof requestAnimationFrame !== "function") {
    cancelGlide(element);
    moveTo(element, axis, target);
    return;
  }

  const durationMs = durationFor(distance);
  let speed = 0;
  if (continuing) {
    const travelled = continuing.target - continuing.from;
    const progress = Math.min(1, (startedAt - continuing.startedAt) / continuing.durationMs);
    // Normalised velocity back into px/ms, then into the new segment's units.
    // Sampled at the same frame `from` came from, so position and speed agree.
    speed = velocityAt(continuing.curve, progress) * travelled / continuing.durationMs;
  }

  cancelGlide(element);
  const glide: Glide = {
    element,
    axis,
    from,
    target,
    startedAt,
    durationMs,
    curve: easeInOut(speed * durationMs / distance),
    lastFrame: startedAt,
    wrote: from,
    frame: 0
  };
  glides.set(element, glide);
  glide.frame = requestAnimationFrame((next) => advance(glide, next));
}

function onWheel(event: WheelEvent) {
  // Someone already handled this deliberately, or it is a zoom gesture.
  if (event.defaultPrevented || event.ctrlKey) return;

  const axis: Axis = Math.abs(event.deltaY) >= Math.abs(event.deltaX) ? "y" : "x";
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
        // scroll at double speed. The exception is a scroller already gliding
        // from an earlier chained tick, reached because the pointer wandered
        // into it: taking the browser's place there continues that motion
        // instead of abandoning it half-way.
        if (latched === null && !isGliding(node, axis)) return;
        glideBy(node, axis, delta);
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
  return () => {
    target.removeEventListener("wheel", handler, { capture: false });
    cancelEveryGlide();
  };
}
