import { afterEach, beforeEach, describe, expect, it } from "vitest";
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

  beforeEach(() => {
    stop = startScrollChaining();
  });

  afterEach(() => {
    stop();
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

    expect(outer.scrollTop).toBe(90);
  });

  it("leaves the browser alone while the inner scroller still has room", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 0 });
    outer.append(inner);
    document.body.append(outer);

    const event = wheel(inner, 40);

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

    expect(inner.scrollTop).toBe(0);
    expect(outer.scrollTop).toBe(50);
  });

  it("ignores a zoom gesture and an already-handled event", () => {
    const outer = scroller({ scrollTop: 0 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 40, { ctrlKey: true });
    expect(outer.scrollTop).toBe(0);

    inner.addEventListener("wheel", (event) => event.preventDefault());
    wheel(inner, 40);
    expect(outer.scrollTop).toBe(0);
  });

  it("converts line and page deltas to pixels before chaining", () => {
    const outer = scroller({ scrollTop: 0, scrollHeight: 5000 });
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    outer.append(inner);
    document.body.append(outer);

    wheel(inner, 3, { deltaMode: WheelEvent.DOM_DELTA_LINE });

    // Three lines, not three pixels: passing the raw value through would make a
    // chained scroll imperceptibly small.
    expect(outer.scrollTop).toBe(48);
  });

  it("does nothing when no ancestor can scroll", () => {
    const inner = scroller({ clientHeight: 50, scrollHeight: 200, scrollTop: 150 });
    document.body.append(inner);

    const event = wheel(inner, 40);

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

    expect(outer.scrollTop).toBe(0);
  });
});
