import { act, renderHook } from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  anyRectOverlaps,
  createBrowserSnapshotStore,
  useBrowserPageProjection,
  type BrowserPageSnapshot,
  type BrowserSnapshotStore
} from "./browserPageProjection";

const SESSION = "session-one";
/** What the host hands back, and the data URL the hook is expected to build out of it. */
const CAPTURE = { data: "UE5H" };
const STILL = "data:image/png;base64,UE5H";
/** jsdom lays nothing out, so the box the frame is measured against is supplied here. */
const PAGE_BOX = { width: 560, height: 656 };
/** The module's own cadences. */
const FRAME_MS = 1_000;
const IDLE_MS = 5_000;
const BACKOFF_MS = 4_000;

/**
 * Every host call and every frame publication, in the order the hook made them.
 *
 * The whole mechanism is an ordering — capture before sink, raise before the frame is dropped —
 * so asserting that the calls happened is asserting almost nothing. Only the sequence is the test.
 */
let log: string[] = [];
/** Frames the hook asked for, run by hand: the two-frame wait is the thing under test. */
let frames: FrameRequestCallback[] = [];
/** The natural size the stubbed decoder reports, so a one-pixel capture can be handed over. */
let decodedSize = { width: 1120, height: 1312 };

type HookProps = {
  contentKey: string | null;
  enabled: boolean;
  hasContent: boolean;
  parked: boolean;
  covered: boolean;
  hostParked: boolean;
  hostCovered: boolean;
};

/**
 * The pane with the pointer inside the page: the one state in which the live page is on top.
 *
 * It is the *exception* in production and the baseline here, because every ordering worth testing
 * is a transition out of it and starting from it leaves the log empty.
 */
function props(overrides: Partial<HookProps> = {}): HookProps {
  return {
    contentKey: SESSION,
    enabled: true,
    hasContent: true,
    parked: false,
    covered: false,
    hostParked: false,
    hostCovered: false,
    ...overrides
  };
}

/**
 * Drains the hook's promise chain: capture, decode and publish are all microtasks.
 *
 * Deliberately not a timer, because the capture loop is on fake ones and a settle that advanced
 * them would fire ticks the test did not ask for.
 */
async function settle(): Promise<void> {
  await act(async () => {
    for (let tick = 0; tick < 16; tick += 1) await Promise.resolve();
  });
}

/** Moves the capture loop's clock, letting each tick's own awaits run out as it goes. */
async function advance(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

/** Runs the frame the hook is waiting on, the way a compositor eventually would. */
function drainFrame(): void {
  const frame = frames.shift();
  if (!frame) throw new Error("the hook asked for no frame");
  frame(0);
}

/** Runs the two frames a sink waits out, then lets the requests behind them go through. */
async function drainSinkFrames(): Promise<void> {
  drainFrame();
  drainFrame();
  await settle();
}

/** The sink, with every publication written into the order log on its way through. */
function loggingSink(): BrowserSnapshotStore {
  const store = createBrowserSnapshotStore();
  return {
    ...store,
    set: (snapshot) => {
      log.push(snapshot ? "publish" : "clear");
      store.set(snapshot);
    }
  };
}

type MountOptions = {
  capture?: () => Promise<{ data: string } | null>;
  setParked?: (parked: boolean) => Promise<unknown>;
  setCovered?: (covered: boolean) => Promise<unknown>;
  /** `null` stands for a placeholder React has rendered but the browser has not laid out. */
  pageBox?: { width: number; height: number } | null;
  initial?: Partial<HookProps>;
};

function mount(options: MountOptions = {}) {
  const box = options.pageBox === undefined ? PAGE_BOX : options.pageBox;
  const element = document.createElement("div");
  element.getBoundingClientRect = () => ({
    x: 0,
    y: 0,
    left: 0,
    top: 0,
    right: box?.width ?? 0,
    bottom: box?.height ?? 0,
    width: box?.width ?? 0,
    height: box?.height ?? 0,
    toJSON: () => ({})
  }) as DOMRect;
  const placeholderRef: RefObject<HTMLElement | null> = { current: element };
  const sink = loggingSink();
  const capture = options.capture ?? (async () => CAPTURE);
  const setParked = options.setParked ?? ((parked: boolean) => {
    log.push(parked ? "sink" : "raise");
    return Promise.resolve();
  });
  const setCovered = options.setCovered ?? ((covered: boolean) => {
    log.push(covered ? "cover" : "uncover");
    return Promise.resolve();
  });
  const view = renderHook(
    (current: HookProps) => useBrowserPageProjection({
      ...current,
      placeholderRef,
      setParked,
      setCovered,
      capture: () => {
        log.push("capture");
        return capture();
      },
      sink
    }),
    { initialProps: props(options.initial) }
  );
  // What mounting itself did, kept for the cases that are about mounting; the orderings the rest
  // of the file is about all start from an empty log.
  const mountLog = [...log];
  log.length = 0;
  return { ...view, sink, mountLog };
}

beforeEach(() => {
  log = [];
  frames = [];
  decodedSize = { width: 1120, height: 1312 };
  // Only the two the capture loop uses. Faking the animation frame as well would collide with the
  // stub below, which is what gives the tests the two-frame wait one tick at a time.
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => frames.push(callback));
  vi.stubGlobal("cancelAnimationFrame", () => {});
  // jsdom decodes nothing, so an `<img>` there always measures zero and would be rejected as the
  // unpainted single pixel the hook is guarding against.
  vi.stubGlobal("Image", class StubImage {
    src = "";
    get naturalWidth() { return decodedSize.width; }
    get naturalHeight() { return decodedSize.height; }
    decode() { return Promise.resolve(); }
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("resting on the projection", () => {
  /**
   * Sunk is the resting state, not the exceptional one: a native child window paints over every
   * HTML layer in the app, so a page left on top is a rectangle React does not control sitting on
   * top of the pane's corners, its transitions and every menu the rest of the app opens.
   */
  it("sinks the page it is handed, and paints a frame before it goes", async () => {
    const view = mount({ initial: { parked: true } });
    // The capture is asked for in the same tick the hook is first handed the page, before the
    // frame it produces can be published.
    expect(view.mountLog).toEqual(["capture"]);
    await settle();
    expect(log).toEqual(["publish"]);
    expect(view.sink.get()?.src).toBe(STILL);

    await drainSinkFrames();
    expect(log).toEqual(["publish", "sink"]);
  });

  /**
   * The projection is what the user is looking at, so it has to keep up with the page — a preview
   * that froze the moment the pointer left it would not show the reload the model just triggered.
   */
  it("refreshes the projection while it is what is on screen", async () => {
    let payload = "AAAA";
    const view = mount({
      initial: { parked: true },
      capture: async () => ({ data: payload })
    });
    await settle();
    await drainSinkFrames();
    log.length = 0;

    payload = "BBBB";
    await advance(FRAME_MS);
    expect(log).toEqual(["capture", "publish"]);
    expect(view.sink.get()?.src).toBe("data:image/png;base64,BBBB");

    payload = "CCCC";
    await advance(FRAME_MS);
    expect(view.sink.get()?.src).toBe("data:image/png;base64,CCCC");
  });

  /**
   * An `<img>` whose source is rewritten is an `<img>` the browser decodes and repaints. Doing
   * that once a second with the same bytes is the projection flickering for no reason, on exactly
   * the page that is sitting still and has nothing to show for it.
   */
  it("leaves the frame alone when the capture says nothing changed", async () => {
    const view = mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();
    const shown = view.sink.get();
    log.length = 0;

    await advance(FRAME_MS);
    await advance(FRAME_MS);

    expect(log.filter((entry) => entry === "publish")).toHaveLength(0);
    expect(view.sink.get()).toBe(shown);
  });

  /**
   * A preview nobody is touching answers with the same bytes forever, and projecting is the state
   * the pane spends nearly all of its life in. Recognising a page that has stopped changing costs
   * a string compare and is the difference between one capture a second, all day, and one every
   * five — on the same WebView2 UI thread the Agent's automation runs on.
   */
  it("drops to the idle cadence once the page stops changing", async () => {
    mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();
    log.length = 0;

    // The frame published on the way down counts as the first; five identical ones after it are
    // what the loop needs to believe nothing is happening.
    for (let tick = 0; tick < 5; tick += 1) await advance(FRAME_MS);
    expect(log.filter((entry) => entry === "capture")).toHaveLength(5);

    log.length = 0;
    await advance(FRAME_MS);
    expect(log).toEqual([]);
    await advance(IDLE_MS - FRAME_MS);
    expect(log.filter((entry) => entry === "capture")).toHaveLength(1);
  });

  /**
   * Capturing fails for ordinary reasons — the page is suspended, being created, or busy — and an
   * older picture of the same page is still a better stand-in than the pane's bare background.
   */
  it("keeps the frame it had when a capture yields nothing, and backs off rather than hammering", async () => {
    let captured: { data: string } | null = CAPTURE;
    const view = mount({ initial: { parked: true }, capture: async () => captured });
    await settle();
    await drainSinkFrames();
    expect(view.sink.get()?.src).toBe(STILL);
    log.length = 0;

    captured = null;
    // Three failures still run at full rate; the fourth is what earns the backoff.
    for (let tick = 0; tick < 4; tick += 1) await advance(FRAME_MS);
    expect(log).toEqual(["capture", "capture", "capture", "capture"]);
    expect(view.sink.get()?.src).toBe(STILL);

    log.length = 0;
    await advance(FRAME_MS);
    expect(log).toEqual([]);
    await advance(BACKOFF_MS - FRAME_MS);
    expect(log).toEqual(["capture"]);
  });

  /**
   * A frozen frame behind a dialog nobody can see past is worth nothing, and each capture crosses
   * CDP and the single WebView2 UI thread to produce it.
   */
  it("stops refreshing while a surface is covering the page", async () => {
    const view = mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();
    log.length = 0;

    view.rerender(props({ parked: true, covered: true }));
    await settle();
    log.length = 0;

    await advance(IDLE_MS * 2);
    expect(log).toEqual([]);
  });

  /**
   * A pane showing its own start page has a native `about:blank` under an opaque, full-bleed card.
   * A picture of that is a picture of nothing, and taking it costs a screenshot a second.
   */
  it("holds no frame and captures nothing while the pane is showing its own card", async () => {
    const view = mount({ initial: { parked: true, hasContent: false } });
    await settle();

    // Sunk in the same tick, with nothing asked of the page and no paint to wait for first.
    expect(view.mountLog).toContain("sink");
    expect(view.mountLog).not.toContain("capture");
    expect(view.mountLog).not.toContain("publish");
    expect(log).toEqual([]);
    expect(view.sink.get()).toBeNull();
    expect(frames).toHaveLength(0);

    log.length = 0;
    await advance(IDLE_MS * 2);
    expect(log).toEqual([]);
  });

  /** A frame is a picture of the page; a card swapped in over it makes that picture a lie. */
  it("drops the frame when the pane swaps the page for a card", async () => {
    const view = mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();
    expect(view.sink.get()?.src).toBe(STILL);
    log.length = 0;

    view.rerender(props({ parked: true, hasContent: false }));
    await settle();

    expect(log).toContain("clear");
    expect(view.sink.get()).toBeNull();
  });

  /** A timer outliving the pane would capture a page for a component that is no longer there. */
  it("leaves no refresh running after the pane goes away", async () => {
    const view = mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();

    view.unmount();
    log.length = 0;
    await advance(IDLE_MS * 2);

    expect(log).toEqual([]);
  });
});

describe("raising the page for the user", () => {
  /**
   * Raising is what keeps this a browser rather than a picture of one. Scrolling, text selection,
   * the IME and native right-click all belong to the real page, and none of them survives being
   * simulated — so the real page is what the user gets for as long as they are using it.
   */
  it("brings the page back before dropping the frame, so the pane's background never shows", async () => {
    const raised: (() => void)[] = [];
    const view = mount({
      initial: { parked: true },
      setParked: (parked) => {
        log.push(parked ? "sink" : "raise");
        return parked ? Promise.resolve() : new Promise<void>((resolve) => { raised.push(resolve); });
      }
    });
    await settle();
    await drainSinkFrames();
    expect(view.sink.get()?.src).toBe(STILL);
    log.length = 0;
    frames.length = 0;

    view.rerender(props({ parked: false }));
    await settle();

    // Asking is not the same as it having happened: until the host answers, the page is still
    // behind the renderer and the `<img>` is the only thing drawing it.
    expect(log).toEqual(["raise"]);
    expect(frames).toHaveLength(0);
    expect(view.sink.get()?.src).toBe(STILL);

    raised.at(-1)?.();
    await settle();
    // One frame, for the paint the page comes back in — the `<img>` has to outlive it.
    expect(frames).toHaveLength(1);
    expect(view.sink.get()?.src).toBe(STILL);

    drainFrame();
    expect(log).toEqual(["raise", "clear"]);
    expect(view.sink.get()).toBeNull();
  });

  /**
   * The host sinks the page for either flag, so a cover left standing would hold it down after it
   * had been asked to come up — and the pane would be showing a raised page's empty box.
   */
  it("drops the cover before asking for the page back", async () => {
    const view = mount({ initial: { parked: true, covered: true } });
    await settle();
    await drainSinkFrames();
    log.length = 0;
    frames.length = 0;

    view.rerender(props({ parked: false, covered: false }));
    await settle();

    expect(log).toEqual(["uncover", "raise"]);
    drainFrame();
    expect(log).toEqual(["uncover", "raise", "clear"]);
  });

  /**
   * Suspending, hiding and closing all take the page away without the pane's state moving. Left
   * believing it still has a page to project, the hook would go on painting a still of something
   * that no longer exists.
   */
  it("gives the page back when there is no longer a page to sink", async () => {
    const view = mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();
    log.length = 0;
    frames.length = 0;

    view.rerender(props({ enabled: false, parked: true }));
    await settle();

    expect(log).toEqual(["raise"]);
    drainFrame();
    expect(log).toEqual(["raise", "clear"]);
    expect(view.sink.get()).toBeNull();
  });

  /**
   * The page remnant.
   *
   * A pane goes away because the browser is being hidden or closed, and the host sinks and hides
   * the page on that path itself. A raise sent from here puts the native window back on top of the
   * z-order at exactly that moment, and whichever of the two lands last wins: a live page, painted
   * over the whole app, with no pane left that could ever take it down again.
   */
  it("leaves the page's stacking alone when the pane goes away, rather than racing the host", async () => {
    const view = mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();
    log.length = 0;

    view.unmount();
    await settle();

    expect(log).toEqual(["clear"]);
    expect(view.sink.get()).toBeNull();
  });

  /**
   * A frame is a picture of one session's page. Switching sessions cannot be repaired by
   * re-measuring the way geometry can — the still would be a different page, which is a lie rather
   * than a stale image — so it is dropped outright.
   */
  it("drops the frame when the page it is a picture of is replaced", async () => {
    const view = mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();
    expect(view.sink.get()?.contentKey).toBe(SESSION);
    log.length = 0;

    view.rerender(props({ contentKey: "session-two", parked: true }));
    await settle();

    expect(log).toContain("clear");
    expect(view.sink.get()).toBeNull();
  });
});

describe("covering the page", () => {
  /**
   * The whole point of keeping a frame ready: covering is a publish and two animation frames, with
   * nothing to wait for. A capture asked for here is a menu the page paints over until it answers.
   */
  it("stands the ready frame in at once, without waiting on a capture the menu would be behind", async () => {
    const view = mount();
    await advance(FRAME_MS);
    log.length = 0;

    view.rerender(props({ parked: true, covered: true }));
    await settle();

    expect(log).toEqual(["publish"]);
    expect(view.sink.get()).toEqual({
      contentKey: SESSION,
      src: STILL,
      data: CAPTURE.data,
      // CSS pixels off the placeholder, not the PNG's own device-pixel size.
      width: PAGE_BOX.width,
      height: PAGE_BOX.height
    });

    // Two frames, not one: the first commits the `<img>`, the second is the one it paints in.
    // Sinking after the first would take the page away before its stand-in was on screen.
    drainFrame();
    expect(log).toEqual(["publish"]);
    drainFrame();
    await settle();
    // And the cover rides behind the sink, never in front of it: the host sinks for either flag,
    // so sending it any earlier would take the page away before the frame had been painted.
    expect(log).toEqual(["publish", "sink", "cover"]);
  });

  /**
   * Resting is not covering. Covering hands the page back to the user and takes it out of agent
   * automation, because the user is looking at a dialog and an Agent click landing behind it would
   * be a click nobody could see. Resting is the pane's normal state, and telling the host it was a
   * cover would mean the Agent lost the page every time the pointer moved.
   */
  it("tells the host a rest is not a cover", async () => {
    const view = mount();
    view.rerender(props({ parked: true }));
    await settle();
    await drainSinkFrames();

    expect(log).toContain("sink");
    expect(log).not.toContain("cover");
  });

  /** A surface opening over a page that is already down moves nothing, so there is nothing to order. */
  it("sends the cover on its own when the page is already down", async () => {
    const view = mount({ initial: { parked: true } });
    await settle();
    await drainSinkFrames();
    log.length = 0;
    frames.length = 0;

    view.rerender(props({ parked: true, covered: true }));
    await settle();

    expect(log).toEqual(["cover"]);
    expect(frames).toHaveLength(0);

    view.rerender(props({ parked: true, covered: false }));
    await settle();
    expect(log).toEqual(["cover", "uncover"]);
  });

  /**
   * The first cover after a page opens comes before the loop has had a second to run, and a page
   * that has failed every capture so far never gets a ready frame at all. Slower, but it still
   * ends with a still on screen and the page behind it.
   */
  it("captures on the spot when no frame was ready, and still paints before sinking", async () => {
    const view = mount();

    view.rerender(props({ parked: true, covered: true }));
    await settle();

    expect(log).toEqual(["capture", "publish"]);
    expect(view.sink.get()?.src).toBe(STILL);
    await drainSinkFrames();
    expect(log).toEqual(["capture", "publish", "sink", "cover"]);
  });

  /**
   * A page left on top of a dialog is the one outcome with no way out of itself: the dialog is
   * unreadable and unclickable, and nothing on screen can bring it forward. A missing still costs
   * an empty box for a moment, so every way of failing to get one still ends in the page sunk.
   */
  it("sinks the page even when nothing could be captured to stand in for it", async () => {
    const captures: [string, () => Promise<{ data: string } | null>][] = [
      ["the host refused", () => Promise.reject(new Error("the embedded browser is not open"))],
      ["there was nothing composited to read", async () => null],
      ["the capture came back empty", async () => ({ data: "" })]
    ];

    for (const [why, capture] of captures) {
      frames.length = 0;
      const view = mount({ capture });

      view.rerender(props({ parked: true, covered: true }));
      await settle();

      expect(log, why).toEqual(["capture", "sink", "cover"]);
      // Nothing was published, so there is no paint to wait for before sinking.
      expect(frames, why).toHaveLength(0);
      view.unmount();
      log.length = 0;
    }
  });

  /**
   * A surface that was never painting answers a capture with one pixel rather than an error.
   * Stretched over the page it would read as a rendering fault; an empty box reads as "no frame".
   */
  it("refuses a one-pixel capture as a stand-in and sinks without one", async () => {
    decodedSize = { width: 1, height: 1 };
    const view = mount();

    view.rerender(props({ parked: true, covered: true }));
    await settle();

    expect(log).toEqual(["capture", "sink", "cover"]);
    expect(view.sink.get()).toBeNull();
  });

  /**
   * A frame painted into a box of no size is not a frame, so there is nothing to ask the page for
   * — but the page still has to go under whatever is drawn over it.
   */
  it("sinks without capturing at all when the page box has not been laid out yet", async () => {
    const view = mount({ pageBox: null });

    view.rerender(props({ parked: true, covered: true }));
    await settle();

    expect(log).toEqual(["sink", "cover"]);
    expect(view.sink.get()).toBeNull();
  });
});

describe("reconciling with the host", () => {
  /**
   * A pane mounting onto a page a previous pane left sunk while the pointer was inside it. Nothing
   * else would ever raise it, and the page would simply be invisible: open, owned, navigating, and
   * behind a renderer drawing nothing in its place.
   */
  it("raises a page it finds already sunk, which nothing else would ever put back", async () => {
    const view = mount({ initial: { hostParked: true } });

    expect(view.mountLog).toEqual(["raise"]);
  });

  /**
   * Sleeping, suspending, hiding and re-presenting all restack the page without this hook asking,
   * and none of them is an event it can hear. Left believing the page is still down, the pane would
   * go on painting a projection over a page that is live again.
   */
  it("sinks the page again when the host drops a sink nobody asked it to drop", async () => {
    const view = mount({ pageBox: null });
    view.rerender(props({ parked: true }));
    await settle();
    expect(log).toEqual(["sink"]);

    // The host applies it, so the hook and the host now agree and nothing is re-sent.
    view.rerender(props({ parked: true, hostParked: true }));
    await settle();
    expect(log).toEqual(["sink"]);

    // And then the host lets it go on its own.
    view.rerender(props({ parked: true, hostParked: false }));
    await settle();
    expect(log).toEqual(["sink", "sink"]);
  });

  /** The same drift, on the flag that decides whether the Agent may drive the page. */
  it("re-sends a cover the host dropped on its own", async () => {
    const view = mount({ pageBox: null });
    view.rerender(props({ parked: true, covered: true, hostParked: true, hostCovered: true }));
    await settle();
    log.length = 0;

    view.rerender(props({ parked: true, covered: true, hostParked: true, hostCovered: false }));
    await settle();

    expect(log).toEqual(["cover"]);
  });

  /**
   * The host's answer legitimately lags the request by the width of an IPC round trip, so a status
   * poll landing inside that window reports the old value. Without the in-flight count there is
   * nothing to tell "has not applied it yet" apart from "dropped it again", and the reconcile
   * re-asks on every 700ms poll for as long as the host takes — or forever, if it is refusing.
   */
  it("sends nothing while its last request is still unanswered", async () => {
    const answered: (() => void)[] = [];
    const view = mount({
      pageBox: null,
      setParked: (parked) => {
        log.push(parked ? "sink" : "raise");
        return new Promise<void>((resolve) => { answered.push(resolve); });
      }
    });
    view.rerender(props({ parked: true }));
    await settle();
    expect(log).toEqual(["sink"]);

    // The host is still reporting what it had before the request, and then reports it again.
    view.rerender(props({ parked: true, hostParked: true }));
    await settle();
    view.rerender(props({ parked: true, hostParked: false }));
    await settle();
    expect(log).toEqual(["sink"]);

    // Once the answer lands the same disagreement is real again, and is acted on.
    answered.shift()?.();
    await settle();
    view.rerender(props({ parked: true, hostParked: true }));
    await settle();
    view.rerender(props({ parked: true, hostParked: false }));
    await settle();
    expect(log).toEqual(["sink", "sink"]);
  });

  /**
   * The two animation frames a sink waits out are a window in which the hook has decided to sink,
   * has not asked yet, and the host truthfully still says the page is up. A reconcile that could
   * not see that window would fire inside it and sink the page early — before the `<img>` standing
   * in for it had been painted, which is the one ordering the whole mechanism exists to get right.
   */
  it("holds off while a transition has decided but not yet asked", async () => {
    const view = mount();
    view.rerender(props({ parked: true, covered: true }));
    await settle();
    expect(log).toEqual(["capture", "publish"]);

    // A status poll lands mid-transition, reporting exactly what is still true.
    view.rerender(props({ parked: true, covered: true, hostParked: false, hostCovered: false }));
    await settle();
    expect(log).toEqual(["capture", "publish"]);

    await drainSinkFrames();
    expect(log).toEqual(["capture", "publish", "sink", "cover"]);
  });

  /**
   * The drift that begins inside the very window nothing may be sent in.
   *
   * A host that drops a sink while a request is still unanswered is the worst case of all: the
   * pane is painting a projection over a page that is live again, and the only thing that could
   * have told it so has already been skipped. Arming the repair and making it once the window
   * closes is what keeps that from being permanent.
   */
  it("makes a repair that was owed from inside the window it could not be sent in", async () => {
    const answered: (() => void)[] = [];
    const view = mount({
      pageBox: null,
      setParked: (parked) => {
        log.push(parked ? "sink" : "raise");
        return new Promise<void>((resolve) => { answered.push(resolve); });
      }
    });
    view.rerender(props({ parked: true }));
    await settle();
    expect(log).toEqual(["sink"]);

    // The host confirms, then drops it again — both while the request is still unanswered.
    view.rerender(props({ parked: true, hostParked: true }));
    await settle();
    view.rerender(props({ parked: true, hostParked: false }));
    await settle();
    expect(log).toEqual(["sink"]);

    // Nothing about the host's answer changes from here; only the window closes.
    answered.shift()?.();
    await settle();
    view.rerender(props({ parked: true, hostParked: false }));
    await settle();

    expect(log).toEqual(["sink", "sink"]);
  });

  /** There is nothing to restack when there is no page, and the host is the one that took it. */
  it("sends nothing while there is no page", async () => {
    const view = mount({ initial: { enabled: false, hostParked: true } });

    expect(view.mountLog).toEqual([]);
  });
});

describe("the snapshot store", () => {
  const frame: BrowserPageSnapshot = {
    contentKey: SESSION,
    src: STILL,
    data: CAPTURE.data,
    width: PAGE_BOX.width,
    height: PAGE_BOX.height
  };

  it("wakes its subscribers once per change and not at all for the same frame", () => {
    const store = createBrowserSnapshotStore();
    const listener = vi.fn();
    const unsubscribe = store.subscribe(listener);

    store.set(frame);
    expect(store.get()).toBe(frame);
    expect(listener).toHaveBeenCalledTimes(1);

    // The `<img>` is the only subscriber, but it lives inside the panel that owns the toolbar,
    // the menu and the drawer: a redundant wake is a redundant render of all of them.
    store.set(frame);
    expect(listener).toHaveBeenCalledTimes(1);

    store.set(null);
    expect(listener).toHaveBeenCalledTimes(2);

    unsubscribe();
    store.set(frame);
    expect(listener).toHaveBeenCalledTimes(2);
  });
});

describe("anyRectOverlaps", () => {
  function rect(left: number, top: number, width: number, height: number) {
    return { left, top, right: left + width, bottom: top + height, width, height };
  }
  const page = rect(900, 164, 560, 656);

  it("answers for the whole set, because the page goes under all of them at once", () => {
    const sidebarMenu = rect(24, 300, 240, 320);
    expect(anyRectOverlaps([sidebarMenu], page)).toBe(false);
    expect(anyRectOverlaps([sidebarMenu, rect(1175, 168, 270, 352)], page)).toBe(true);
    expect(anyRectOverlaps([], page)).toBe(false);
  });

  /** A surface React has rendered but the browser has not laid out measures zero, not "origin". */
  it("reads an unmeasured rectangle as no rectangle at all", () => {
    expect(anyRectOverlaps([rect(1000, 300, 0, 0)], page)).toBe(false);
    expect(anyRectOverlaps([rect(1000, 300, 200, 120)], rect(0, 0, 0, 0))).toBe(false);
  });

  /** A surface flush against the page's edge covers none of it. */
  it("treats a shared edge as no overlap", () => {
    expect(anyRectOverlaps([rect(700, 300, 200, 120)], page)).toBe(false);
    expect(anyRectOverlaps([rect(701, 300, 200, 120)], page)).toBe(true);
  });
});
