import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { startScrollChaining } from "./scrollChaining";

/**
 * jsdom has no layout, so `scrollHeight`/`clientHeight` are always 0 and
 * `scrollTop` never clamps. This builds an element that reports the geometry of
 * a real scroller and clamps assignments the way a browser does, which is the
 * behavior the handler actually reasons about.
 */
function scroller({
  overflow = "auto",
  overscrollBehavior = "auto",
  clientHeight = 100,
  scrollHeight = 300,
  scrollTop = 0
}: {
  overflow?: string;
  overscrollBehavior?: string;
  clientHeight?: number;
  scrollHeight?: number;
  scrollTop?: number;
} = {}): HTMLElement {
  const element = document.createElement("div");
  element.style.overflowY = overflow;
  element.style.overscrollBehaviorY = overscrollBehavior;
  Object.defineProperty(element, "clientHeight", { value: clientHeight, configurable: true });
  Object.defineProperty(element, "scrollHeight", { value: scrollHeight, configurable: true });
  let position = scrollTop;
  Object.defineProperty(element, "scrollTop", {
    configurable: true,
    get: () => position,
    set: (next: number) => {
      position = Math.max(0, Math.min(next, scrollHeight - clientHeight));
    }
  });
  return element;
}

/** A non-scrolling wrapper, so the chain has to walk past ordinary elements. */
function plain(): HTMLElement {
  const element = document.createElement("div");
  Object.defineProperty(element, "clientHeight", { value: 100, configurable: true });
  Object.defineProperty(element, "scrollHeight", { value: 100, configurable: true });
  return element;
}

function wheel(target: Element, deltaY: number, init: WheelEventInit = {}): WheelEvent {
  const event = new WheelEvent("wheel", {
    deltaY,
    bubbles: true,
    cancelable: true,
    ...init
  });
  target.dispatchEvent(event);
  return event;
}

describe("scroll chaining", () => {
  let stop: () => void;
  /**
   * A chained scroll is animated, so the tests need a clock and a frame loop
   * they can step. jsdom's own rAF fires on a real timer, which would make
   * every assertion about the middle of a glide a race.
   */
  let clock = 0;
  let nextFrameId = 1;
  let pending = new Map<number, FrameRequestCallback>();

  /** Runs whatever frames are due, `ms` later, after `before` has had its say. */
  function frame(ms = 16, before?: () => void): void {
    clock += ms;
    before?.();
    const due = [...pending.values()];
    pending.clear();
    for (const callback of due) callback(clock);
  }

  /** Runs frames until nothing is animating any more. */
  function settle(): void {
    for (let step = 0; step < 200 && pending.size > 0; step += 1) frame();
  }

  beforeEach(() => {
    clock = 0;
    nextFrameId = 1;
    pending = new Map();
    vi.spyOn(performance, "now").mockImplementation(() => clock);
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      const id = nextFrameId;
      nextFrameId += 1;
      pending.set(id, callback);
      return id;
    });
    vi.stubGlobal("cancelAnimationFrame", (id: number) => {
      pending.delete(id);
    });
    stop = startScrollChaining();
  });

  afterEach(() => {
    stop();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    document.body.innerHTML = "";
  });

  it("carries the leftover delta to the parent when the inner scroller is at its end", () => {
    const outer = scroller({ scrollTop: 40 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    // The inner scroller is pinned at its bottom (200 - 50 = 150), which is
    // exactly where Chromium's latch stalls the gesture.
    const event = wheel(inner, 60);
    settle();

    expect(inner.scrollTop).toBe(150);
    expect(outer.scrollTop).toBe(100);
    expect(event.defaultPrevented).toBe(true);
  });

  it("chains upward on every wheel tick without waiting for the pointer to move", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    // The bug: a second tick from an unmoved cursor did nothing. Three
    // back-to-back ticks on the same target must each move the parent.
    wheel(inner, 30);
    wheel(inner, 30);
    wheel(inner, 30);
    settle();

    expect(outer.scrollTop).toBe(90);
  });

  it("animates the chained delta instead of jumping to it", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 120);

    // The wheel event itself moves nothing. The parent has to travel over
    // frames the way the browser would have moved it, or the gesture visibly
    // changes character at the boundary — which is the whole complaint.
    expect(outer.scrollTop).toBe(0);

    frame();
    const afterOneFrame = outer.scrollTop;
    expect(afterOneFrame).toBeGreaterThan(0);
    expect(afterOneFrame).toBeLessThan(120);

    frame();
    expect(outer.scrollTop).toBeGreaterThan(afterOneFrame);

    settle();
    expect(outer.scrollTop).toBe(120);
  });

  it("spreads a small tick over Chromium's full 200ms, not one frame", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 120);
    for (let step = 0; step < 6; step += 1) frame(16);

    // 96ms in, still travelling.
    expect(outer.scrollTop).toBeLessThan(120);

    for (let step = 0; step < 7; step += 1) frame(16);
    expect(outer.scrollTop).toBe(120);
  });

  it("shortens the glide for a large tick, the way the wheel curve does", () => {
    const outer = scroller({ scrollTop: 0, scrollHeight: 5000 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    // Past the 480px ramp end, so the duration is 100ms rather than 200ms and
    // a fast flick does not trail behind the wheel.
    wheel(inner, 600);
    for (let step = 0; step < 7; step += 1) frame(16);

    expect(outer.scrollTop).toBe(600);
  });

  it("adds a tick that lands mid-glide to the target instead of restarting", () => {
    const outer = scroller({ scrollTop: 0, scrollHeight: 5000 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 100);
    frame();
    frame();
    const mid = outer.scrollTop;
    expect(mid).toBeGreaterThan(0);
    expect(mid).toBeLessThan(100);

    // Retargeting from the current position would lose whatever the first tick
    // had not travelled yet, so a held wheel would scroll short.
    wheel(inner, 100);
    settle();

    expect(outer.scrollTop).toBe(200);
  });

  it("never stands still for a frame while the wheel is held", () => {
    const outer = scroller({ scrollTop: 0, scrollHeight: 5000 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    const TICKS = 5;
    const perFrame: number[] = [];
    let previous = 0;
    for (let step = 1; step <= 40; step += 1) {
      // A held wheel, one tick every third frame. `requestAnimationFrame`
      // reports the time the frame began and the tick is dispatched inside that
      // same frame, so a segment stamped "now" is still at progress zero when
      // the callback runs — the scroller would stand still for one frame at
      // every tick, which is the stutter the anchoring exists to remove.
      const tick = step % 3 === 1 && (step - 1) / 3 < TICKS;
      frame(16, tick ? () => wheel(inner, 100) : undefined);
      perFrame.push(outer.scrollTop - previous);
      previous = outer.scrollTop;
    }

    expect(outer.scrollTop).toBe(100 * TICKS);
    while (perFrame.length > 0 && perFrame[perFrame.length - 1] === 0) perFrame.pop();
    // The first frame carries the first tick and has no elapsed time behind it
    // yet; every frame after it, until the glide runs out, has to move.
    expect(perFrame.length).toBeGreaterThan(12);
    expect(perFrame.slice(1).filter((moved) => moved <= 0)).toEqual([]);
  });

  it("gets out of the way when something else scrolls the same element", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 120);
    frame();
    expect(outer.scrollTop).toBeGreaterThan(0);

    // A pane sticking itself to the bottom, a `scrollIntoView`, the browser's
    // own scrolling once the pointer moved: two writers per frame is jitter, so
    // the glide stands down rather than dragging the offset back.
    outer.scrollTop = 200;
    settle();

    expect(outer.scrollTop).toBe(200);
  });

  it("keeps driving a scroller the pointer wanders into mid-glide", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 60);
    frame();

    // The pointer has left the child, so the browser would latch onto the
    // parent and scroll it natively from wherever the glide has got to —
    // dropping the rest of the glide and stuttering. Taking over continues it.
    const event = wheel(outer, 60);
    expect(event.defaultPrevented).toBe(true);
    settle();

    expect(outer.scrollTop).toBe(120);
  });

  it("leaves the browser alone while the inner scroller still has room", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 0 });
    outer.append(inner);
    document.body.append(outer);

    const event = wheel(inner, 40);
    settle();

    // Not prevented, and neither element was moved by us: the browser's own
    // scrolling is correct here, and double-applying would scroll twice as fast.
    expect(event.defaultPrevented).toBe(false);
    expect(inner.scrollTop).toBe(0);
    expect(outer.scrollTop).toBe(0);
  });

  it("chains upward past non-scrolling wrappers", () => {
    const outer = scroller({ scrollTop: 0 });
    const middle = plain();
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    middle.append(inner);
    outer.append(middle);
    document.body.append(outer);

    wheel(inner, 25);
    settle();

    expect(outer.scrollTop).toBe(25);
  });

  it("stops at a scroller that asks to contain its overscroll", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({
      clientHeight: 50,
      scrollHeight: 200,
      scrollTop: 150,
      overscrollBehavior: "contain"
    });
    outer.append(inner);
    document.body.append(outer);

    const event = wheel(inner, 40);
    settle();

    // `contain` is a deliberate boundary, so the parent must not move.
    expect(outer.scrollTop).toBe(0);
    expect(event.defaultPrevented).toBe(false);
  });

  it("chains upward when scrolling back up as well as down", () => {
    const outer = scroller({ scrollTop: 80 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 0 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, -30);
    settle();

    expect(inner.scrollTop).toBe(0);
    expect(outer.scrollTop).toBe(50);
  });

  it("chains past a parent whose glide is already aimed at its end", () => {
    const outer = scroller({ scrollTop: 0, clientHeight: 100, scrollHeight: 200 });
    const middle = scroller({ clientHeight: 50, scrollHeight: 100, scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    middle.append(inner);
    outer.append(middle);
    document.body.append(outer);

    // Fills the middle scroller's remaining 50px, then keeps going while it is
    // still travelling: room is measured from where it is headed, so the second
    // tick goes to the grandparent rather than piling onto a spent target.
    wheel(inner, 50);
    frame();
    wheel(inner, 40);
    settle();

    expect(middle.scrollTop).toBe(50);
    expect(outer.scrollTop).toBe(40);
  });

  it("ignores a zoom gesture and an already-handled event", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 40, { ctrlKey: true });
    settle();
    expect(outer.scrollTop).toBe(0);

    inner.addEventListener("wheel", (event) => event.preventDefault());
    wheel(inner, 40);
    settle();
    expect(outer.scrollTop).toBe(0);
  });

  it("converts line and page deltas to pixels before chaining", () => {
    const outer = scroller({ scrollTop: 0, scrollHeight: 5000 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 3, { deltaMode: WheelEvent.DOM_DELTA_LINE });
    settle();

    // Three lines, not three pixels: passing the raw value through would make a
    // chained scroll imperceptibly small.
    expect(outer.scrollTop).toBe(48);
  });

  it("does nothing when no ancestor can scroll", () => {
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    document.body.append(inner);

    const event = wheel(inner, 40);
    settle();

    expect(event.defaultPrevented).toBe(false);
    expect(inner.scrollTop).toBe(150);
  });

  it("stops chaining once uninstalled", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    stop();
    wheel(inner, 40);
    settle();

    expect(outer.scrollTop).toBe(0);
  });
});
