import { act, renderHook } from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pointerIsInsideRect, useBrowserPageEngagement } from "./browserPageEngagement";

/** The page box the pane publishes; jsdom lays nothing out, so it is supplied here. */
const PAGE = { left: 900, top: 164, width: 560, height: 656 };

let frames: FrameRequestCallback[] = [];

function pageElement(): HTMLElement {
  const element = document.createElement("div");
  element.getBoundingClientRect = () => ({
    x: PAGE.left,
    y: PAGE.top,
    left: PAGE.left,
    top: PAGE.top,
    right: PAGE.left + PAGE.width,
    bottom: PAGE.top + PAGE.height,
    width: PAGE.width,
    height: PAGE.height,
    toJSON: () => ({})
  }) as DOMRect;
  return element;
}

function mount(enabled = true) {
  const element = pageElement();
  const pageAreaRef: RefObject<HTMLElement | null> = { current: element };
  const view = renderHook(
    (current: { enabled: boolean }) => useBrowserPageEngagement({
      pageAreaRef,
      enabled: current.enabled
    }),
    { initialProps: { enabled } }
  );
  return { ...view, element };
}

/** A pointer event the renderer actually received, at viewport coordinates. */
function pointer(type: string, x: number, y: number): void {
  act(() => {
    window.dispatchEvent(new MouseEvent(type, { clientX: x, clientY: y, bubbles: true }));
  });
}

/** Runs the frame a coalesced `pointermove` is waiting on. */
function drainFrames(): void {
  const pending = frames;
  frames = [];
  act(() => {
    for (const frame of pending) frame(0);
  });
}

function windowEvent(type: string): void {
  act(() => {
    window.dispatchEvent(new Event(type));
  });
}

const INSIDE: [number, number] = [PAGE.left + 20, PAGE.top + 20];
const OUTSIDE: [number, number] = [PAGE.left - 40, PAGE.top + 20];

beforeEach(() => {
  frames = [];
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frames.push(callback);
    return frames.length;
  });
  vi.stubGlobal("cancelAnimationFrame", () => {});
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("deciding when the page is being used", () => {
  /** Sunk is the resting state, so nothing has to happen for the projection to be what is shown. */
  it("starts disengaged", () => {
    expect(mount().result.current).toBe(false);
  });

  it("engages when the pointer is inside the page and lets go when it is elsewhere", () => {
    const view = mount();

    pointer("pointermove", ...INSIDE);
    drainFrames();
    expect(view.result.current).toBe(true);

    pointer("pointermove", ...OUTSIDE);
    drainFrames();
    expect(view.result.current).toBe(false);
  });

  /**
   * The inversion this module exists for.
   *
   * The page is a native child window: the instant it is raised it takes every pointer event over
   * its own rectangle, so the page element sees the pointer *leave* as a consequence of engaging.
   * A `pointerleave` read as "the user is done" would sink the page in the same breath as raising
   * it, forever.
   */
  it("ignores the page element's own pointerleave, which fires because the page was raised", () => {
    const view = mount();
    pointer("pointerover", ...INSIDE);
    expect(view.result.current).toBe(true);

    act(() => {
      view.element.dispatchEvent(new MouseEvent("pointerleave", { bubbles: true }));
      view.element.dispatchEvent(new MouseEvent("pointerout", { bubbles: true }));
      document.dispatchEvent(new MouseEvent("mouseleave", { bubbles: true }));
    });

    expect(view.result.current).toBe(true);
  });

  /** A press has to decide before it lands; waiting a frame is waiting past the click. */
  it("engages on the crossing itself, without waiting for a frame", () => {
    const view = mount();

    pointer("pointerover", ...INSIDE);
    expect(view.result.current).toBe(true);

    pointer("pointerdown", ...OUTSIDE);
    expect(view.result.current).toBe(false);
  });

  /** One layout read per frame: a pointer crossing the window outruns any restack by far. */
  it("coalesces a burst of moves into a single decision", () => {
    const view = mount();

    pointer("pointermove", ...INSIDE);
    pointer("pointermove", ...INSIDE);
    pointer("pointermove", ...INSIDE);
    expect(frames).toHaveLength(1);

    drainFrames();
    expect(view.result.current).toBe(true);
  });

  /** A page not laid out has no box to be inside of. */
  it("reads an unmeasured page box as no page at all", () => {
    expect(pointerIsInsideRect(10, 10, null)).toBe(false);
    expect(pointerIsInsideRect(10, 10, {
      left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0
    })).toBe(false);
  });

  /** A pointer on the page's own edge is on the app's side of it. */
  it("treats the trailing edge as outside", () => {
    const rect = {
      left: PAGE.left,
      top: PAGE.top,
      right: PAGE.left + PAGE.width,
      bottom: PAGE.top + PAGE.height,
      width: PAGE.width,
      height: PAGE.height
    };
    expect(pointerIsInsideRect(PAGE.left, PAGE.top, rect)).toBe(true);
    expect(pointerIsInsideRect(rect.right, PAGE.top, rect)).toBe(false);
    expect(pointerIsInsideRect(PAGE.left, rect.bottom, rect)).toBe(false);
  });
});

describe("keeping the page up for the keyboard", () => {
  /**
   * Once the user has clicked into the page they may be typing in it, and the pointer wandering
   * off is no longer a statement that they are done. Sinking then would leave them typing into
   * something they cannot see.
   */
  it("stays engaged after the pointer leaves, when the page took the keyboard", () => {
    const view = mount();
    pointer("pointerover", ...INSIDE);
    expect(view.result.current).toBe(true);

    // This webview losing focus while the page was the surface on top is the page taking it.
    windowEvent("blur");
    pointer("pointermove", ...OUTSIDE);
    drainFrames();

    expect(view.result.current).toBe(true);
  });

  /** The app has the keyboard back, so the page does not, and the pointer decides alone again. */
  it("lets go once this webview has the keyboard again", () => {
    const view = mount();
    pointer("pointerover", ...INSIDE);
    windowEvent("blur");
    pointer("pointermove", ...OUTSIDE);
    drainFrames();
    expect(view.result.current).toBe(true);

    windowEvent("focus");
    pointer("pointermove", ...OUTSIDE);
    drainFrames();

    expect(view.result.current).toBe(false);
  });

  /** A press in the app moves focus into it, so the claim is dropped by the same event. */
  it("lets go on a press outside the page, without waiting for a focus event", () => {
    const view = mount();
    pointer("pointerover", ...INSIDE);
    windowEvent("blur");

    pointer("pointerdown", ...OUTSIDE);

    expect(view.result.current).toBe(false);
  });

  /** A blur with the page already down is the app going to the background, not the page. */
  it("does not invent a keyboard claim from a blur while disengaged", () => {
    const view = mount();
    windowEvent("blur");
    pointer("pointermove", ...OUTSIDE);
    drainFrames();

    expect(view.result.current).toBe(false);
  });
});

describe("standing down", () => {
  /** A pane showing its own card, or one that is not the visible pane, has nothing to raise. */
  it("forces disengagement when there is nothing to raise", () => {
    const view = mount();
    pointer("pointerover", ...INSIDE);
    expect(view.result.current).toBe(true);

    view.rerender({ enabled: false });
    expect(view.result.current).toBe(false);

    // And stops listening: a pointer crossing the old page box decides nothing now.
    pointer("pointerover", ...INSIDE);
    expect(view.result.current).toBe(false);
  });

  /** A hidden window is not being used, whatever the last pointer event said. */
  it("lets go when the document stops being visible", () => {
    const view = mount();
    pointer("pointerover", ...INSIDE);
    windowEvent("blur");
    expect(view.result.current).toBe(true);

    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(view.result.current).toBe(false);

    // The keyboard claim went with it, so coming back does not silently raise the page again.
    visibility.mockReturnValue("visible");
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    pointer("pointermove", ...OUTSIDE);
    drainFrames();
    expect(view.result.current).toBe(false);
  });

  /** A listener outliving the pane would keep deciding for a component that is no longer there. */
  it("stops listening when the pane goes away", () => {
    const view = mount();
    const removed: string[] = [];
    const remove = vi.spyOn(window, "removeEventListener")
      .mockImplementation(((type: string) => { removed.push(type); }) as typeof window.removeEventListener);

    view.unmount();

    expect(removed).toEqual(expect.arrayContaining([
      "pointermove",
      "pointerover",
      "pointerdown",
      "blur",
      "focus"
    ]));
    remove.mockRestore();
  });
});
