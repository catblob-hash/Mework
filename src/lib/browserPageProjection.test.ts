import { act, renderHook } from "@testing-library/react";
import { StrictMode, type RefObject } from "react";
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
const FIRST_FRAME_MS = 300;
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
/** Frames the hook asked for, run by hand: the one-frame wait is the thing under test. */
let frames: FrameRequestCallback[] = [];
/** The natural size the stubbed decoder reports, so a one-pixel capture can be handed over. */
let decodedSize = { width: 1120, height: 1312 };

type HookProps = {
  contentKey: string | null;
  enabled: boolean;
  presented: boolean;
  hasContent: boolean;
  parked: boolean;
  covered: boolean;
  hostParked: boolean;
  hostCovered: boolean;
};

/** The page up, with nothing needing it gone: the resting state, and where most cases start. */
function props(overrides: Partial<HookProps> = {}): HookProps {
  return {
    contentKey: SESSION,
    enabled: true,
    presented: true,
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
  // stub below, which is what gives the tests the one-frame wait a tick at a time.
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

describe("declaring on mount", () => {
  /**
   * The pane says where its page belongs the moment it mounts, before the page is created or
   * presented, and the host keeps that through presentation. A pane that waited for the page to
   * exist had it presented on top of its own start card — a blank rectangle — until it caught up;
   * and a pane that assumed the host already agreed left a page sunk by an earlier pane where it
   * was.
   */
  it("says where the page belongs before the page is presented, whichever way that is", async () => {
    const stacking = (entries: string[]) => entries.filter((entry) => entry === "sink" || entry === "raise");
    const down = mount({ initial: { presented: false, parked: true, hasContent: false } });
    expect(stacking(down.mountLog)).toEqual(["sink"]);
    down.unmount();
    log.length = 0;

    const up = mount({ initial: { presented: false } });
    expect(stacking(up.mountLog)).toEqual(["raise"]);
    up.unmount();
  });

  /** A capture of a page that is not presented moves it off screen and back, for nothing. */
  it("captures nothing until the page is presented", async () => {
    const view = mount({ initial: { presented: false } });
    await advance(IDLE_MS * 2);
    expect(log).toEqual([]);

    view.rerender(props({ presented: true }));
    await advance(FIRST_FRAME_MS);
    expect(log).toEqual(["capture"]);
  });

  /** Nor is one taken to fill the box when a page not yet presented is covered. */
  it("covers a page that is not presented without capturing it", async () => {
    const view = mount({ initial: { presented: false } });

    view.rerender(props({ presented: false, parked: true, covered: true }));
    await settle();

    expect(log).toEqual(["sink", "cover"]);
    expect(view.sink.get()).toBeNull();
  });
});

describe("keeping a frame warm while the page is up", () => {
  /**
   * The page is up whenever nothing needs it gone, and the next menu opened across it must not
   * wait a capture round trip to get a still in its place — a menu that waits is a menu the page
   * paints over. So a frame is kept ready, and none is published: the page is showing itself.
   */
  it("captures soon after the page comes up, then once a second, publishing nothing", async () => {
    const view = mount();
    // The pane's first word on its page, sent before anything else (see "declaring on mount").
    expect(view.mountLog).toEqual(["raise"]);

    await advance(FIRST_FRAME_MS);
    expect(log).toEqual(["capture"]);
    await advance(FRAME_MS);
    expect(log).toEqual(["capture", "capture"]);
    expect(view.sink.get()).toBeNull();
  });

  /**
   * A preview nobody is touching answers with the same bytes forever. Recognising a page that has
   * stopped changing costs a string compare and is the difference between one capture a second,
   * all day, and one every five — on the same WebView2 UI thread the Agent's automation runs on.
   */
  it("drops to the idle cadence once the page stops changing", async () => {
    mount();
    await advance(FIRST_FRAME_MS);
    // The first frame is compared against nothing; five identical ones after it are what the loop
    // needs to believe nothing is happening.
    for (let tick = 0; tick < 5; tick += 1) await advance(FRAME_MS);
    expect(log.filter((entry) => entry === "capture")).toHaveLength(6);

    log.length = 0;
    await advance(FRAME_MS);
    expect(log).toEqual([]);
    await advance(IDLE_MS - FRAME_MS);
    expect(log).toEqual(["capture"]);
  });

  /**
   * Capturing fails for ordinary reasons — the page is suspended, being created, or busy — and
   * none of them is worth retrying at full rate.
   */
  it("backs off rather than hammering a page that yields nothing", async () => {
    mount({ capture: async () => null });
    await advance(FIRST_FRAME_MS);
    // Three failures still run at full rate; the fourth is what earns the backoff.
    for (let tick = 0; tick < 3; tick += 1) await advance(FRAME_MS);
    expect(log).toEqual(["capture", "capture", "capture", "capture"]);

    log.length = 0;
    await advance(FRAME_MS);
    expect(log).toEqual([]);
    await advance(BACKOFF_MS - FRAME_MS);
    expect(log).toEqual(["capture"]);
  });

  /**
   * Down, the page is either behind a dialog nobody can see past or out of sight altogether, and
   * each capture crosses CDP and the single WebView2 UI thread to produce pixels nobody sees.
   */
  it("stops capturing while the page is down, whatever the reason", async () => {
    for (const reason of [{ covered: true }, { covered: false }]) {
      frames.length = 0;
      const view = mount();
      await advance(FIRST_FRAME_MS);
      view.rerender(props({ parked: true, ...reason }));
      await settle();
      drainFrame();
      await settle();
      log.length = 0;

      await advance(IDLE_MS * 2);
      expect(log, JSON.stringify(reason)).toEqual([]);
      view.unmount();
      log.length = 0;
    }
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
    expect(view.sink.get()).toBeNull();
    expect(frames).toHaveLength(0);

    log.length = 0;
    await advance(IDLE_MS * 2);
    expect(log).toEqual([]);
  });

  /** A frame is a picture of the page; a card swapped in over it makes that picture a lie. */
  it("drops the frame when the pane swaps the page for a card", async () => {
    const view = mount();
    await advance(FIRST_FRAME_MS);
    view.rerender(props({ parked: true, covered: true }));
    await settle();
    expect(view.sink.get()?.src).toBe(STILL);
    log.length = 0;

    view.rerender(props({ parked: true, covered: true, hasContent: false }));
    await settle();

    expect(log).toContain("clear");
    expect(view.sink.get()).toBeNull();
  });

  /** A timer outliving the pane would capture a page for a component that is no longer there. */
  it("leaves no capture running after the pane goes away", async () => {
    const view = mount();
    await advance(FIRST_FRAME_MS);

    view.unmount();
    log.length = 0;
    await advance(IDLE_MS * 2);

    expect(log).toEqual([]);
  });
});

describe("taking the page down", () => {
  /**
   * The whole point of keeping a frame warm: the still is published from the very layout effect
   * the cover arrives in, so it is painted in the same frame as the surface that covers the page,
   * and the page leaves one animation frame later. Nothing waits on a capture.
   */
  it("stands the warm frame in at once and sinks one frame later", async () => {
    const view = mount();
    await advance(FIRST_FRAME_MS);
    log.length = 0;

    view.rerender(props({ parked: true, covered: true }));
    // Synchronously, before any promise has run: this is the commit the cover was drawn in.
    expect(log).toEqual(["publish"]);
    expect(view.sink.get()).toEqual({
      contentKey: SESSION,
      src: STILL,
      data: CAPTURE.data,
      // CSS pixels off the placeholder, not the PNG's own device-pixel size.
      width: PAGE_BOX.width,
      height: PAGE_BOX.height
    });
    await settle();
    expect(log).toEqual(["publish"]);

    // One frame: the commit that drew the `<img>` is painted by the time it runs.
    expect(frames).toHaveLength(1);
    drainFrame();
    await settle();
    // And the cover rides behind the sink, never in front of it: the host sinks for either flag,
    // so sending it any earlier would take the page away before the frame had been painted.
    expect(log).toEqual(["publish", "sink", "cover"]);
  });

  /**
   * Out of sight is not covered. Covering hands the page back to the user and takes it out of
   * agent automation, because the user is looking at a dialog and an Agent click landing behind it
   * would be a click nobody could see. A pane merely hidden under another must cost the Agent
   * nothing.
   */
  it("tells the host that out of sight is not a cover", async () => {
    const view = mount();
    await advance(FIRST_FRAME_MS);
    view.rerender(props({ parked: true }));
    await settle();
    drainFrame();
    await settle();

    expect(log).toContain("sink");
    expect(log).not.toContain("cover");
  });

  /** A surface opening over a page that is already down moves nothing, so there is nothing to order. */
  it("sends the cover on its own when the page is already down", async () => {
    const view = mount({ pageBox: null, initial: { parked: true } });
    await settle();
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
   * A cover that comes before the first frame has been taken — the page only just came up — has
   * nothing to stand in. Waiting for a capture would leave the page over the menu for the whole
   * round trip, so the page goes down at once and the box is filled when the capture lands.
   */
  it("sinks at once when no frame is warm, and fills the box when a capture lands", async () => {
    const view = mount();

    view.rerender(props({ parked: true, covered: true }));
    expect(log).toEqual(["sink", "capture"]);
    expect(frames).toHaveLength(0);
    await settle();

    expect(log).toEqual(expect.arrayContaining(["cover", "publish"]));
    expect(view.sink.get()?.src).toBe(STILL);
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

      expect(log, why).toEqual(["sink", "capture", "cover"]);
      expect(view.sink.get(), why).toBeNull();
      view.unmount();
      log.length = 0;
    }
  });

  /**
   * A surface that was never painting answers a capture with one pixel rather than an error.
   * Stretched over the page it would read as a rendering fault; an empty box reads as "no frame".
   */
  it("refuses a one-pixel capture as a stand-in", async () => {
    decodedSize = { width: 1, height: 1 };
    const view = mount();

    view.rerender(props({ parked: true, covered: true }));
    await settle();

    expect(log).toEqual(["sink", "capture", "cover"]);
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

  /**
   * Only a newer transition may cancel a sink waiting on its frame. A re-render that changes
   * nothing about where the page belongs — here the page it shows being replaced — used to cancel
   * it through the effect's own cleanup, and the page stayed on top of the menu it was going down
   * for, with nothing left that would ever ask again.
   */
  it("keeps a sink waiting on its frame through a re-render that is not a transition", async () => {
    const view = mount();
    await advance(FIRST_FRAME_MS);
    log.length = 0;

    view.rerender(props({ parked: true, covered: true }));
    view.rerender(props({ parked: true, covered: true, contentKey: "session-two" }));
    await settle();
    drainFrame();
    await settle();

    expect(log).toEqual(expect.arrayContaining(["sink", "cover"]));
  });
});

describe("bringing the page back", () => {
  /** Down under a cover, with a still on screen: where every case here starts. */
  async function coveredView(options: MountOptions = {}) {
    const view = mount(options);
    await advance(FIRST_FRAME_MS);
    view.rerender(props({ parked: true, covered: true }));
    await settle();
    drainFrame();
    await settle();
    expect(view.sink.get()?.src).toBe(STILL);
    log.length = 0;
    frames.length = 0;
    return view;
  }

  it("brings the page back before dropping the frame, so the pane's background never shows", async () => {
    const raised: (() => void)[] = [];
    const view = await coveredView({
      setParked: (parked) => {
        log.push(parked ? "sink" : "raise");
        return parked ? Promise.resolve() : new Promise<void>((resolve) => { raised.push(resolve); });
      }
    });

    view.rerender(props({ parked: false }));
    await settle();

    // Asking is not the same as it having happened: until the host answers, the page is still
    // behind the renderer and the `<img>` is the only thing drawing it.
    expect(log).toEqual(["uncover", "raise"]);
    expect(frames).toHaveLength(0);
    expect(view.sink.get()?.src).toBe(STILL);

    raised.at(-1)?.();
    await settle();
    // One frame, for the paint the page comes back in — the `<img>` has to outlive it.
    expect(frames).toHaveLength(1);
    expect(view.sink.get()?.src).toBe(STILL);

    drainFrame();
    expect(log).toEqual(["uncover", "raise", "clear"]);
    expect(view.sink.get()).toBeNull();
  });

  /**
   * Suspending, hiding and closing all take the page away without the pane's state moving. Left
   * believing it still has a page to stand in for, the hook would go on painting a still of
   * something that no longer exists.
   */
  it("gives the page back when there is no longer a page to sink", async () => {
    const view = await coveredView();

    view.rerender(props({ enabled: false, parked: true, covered: true }));
    await settle();

    expect(log).toEqual(["uncover", "raise"]);
    drainFrame();
    expect(log).toEqual(["uncover", "raise", "clear"]);
    expect(view.sink.get()).toBeNull();
  });

  /**
   * A frame is a picture of one session's page. Switching sessions cannot be repaired by
   * re-measuring the way geometry can — the still would be a different page, which is a lie rather
   * than a stale image — so it is dropped outright.
   */
  it("drops the frame when the page it is a picture of is replaced", async () => {
    const view = await coveredView();

    view.rerender(props({ contentKey: "session-two", parked: true, covered: true }));
    await settle();

    expect(log).toContain("clear");
    expect(view.sink.get()).toBeNull();
  });
});

describe("the pane going away", () => {
  /**
   * The pane goes because the browser is being hidden or closed, or another tab or conversation
   * is taking its place, and the host parks the page on each of those paths — but only once the
   * renderer asks, after the frame in which the pane has already vanished. Taken down from the
   * unmount, the page is gone in that same frame instead of painted over whatever replaced the pane.
   */
  it("takes a page that is up down with it", async () => {
    const view = mount();

    view.unmount();
    await settle();

    expect(log).toEqual(["sink", "clear"]);
  });

  /** A page already down has nothing to be taken out from under, and no raise is sent either. */
  it("leaves a page that is already down where it is", async () => {
    const view = mount({ pageBox: null, initial: { parked: true } });
    await settle();
    log.length = 0;

    view.unmount();
    await settle();

    expect(log).toEqual(["clear"]);
  });

  /** There is nothing to restack when there is no page. */
  it("sends nothing for a page that is not there", async () => {
    const view = mount({ initial: { enabled: false } });

    view.unmount();
    await settle();

    expect(log).toEqual(["clear"]);
  });

  /**
   * React's development double mount runs every cleanup and then every effect again on a pane
   * that never went anywhere. The page the cleanup took down has to come back up, or a freshly
   * opened preview would show nothing at all until the next status poll repaired it.
   */
  it("puts the page back up when the pane only remounts", async () => {
    const element = document.createElement("div");
    const placeholderRef: RefObject<HTMLElement | null> = { current: element };
    const sink = loggingSink();
    renderHook(
      () => useBrowserPageProjection({
        ...props(),
        placeholderRef,
        setParked: (parked) => {
          log.push(parked ? "sink" : "raise");
          return Promise.resolve();
        },
        setCovered: () => Promise.resolve(),
        capture: async () => CAPTURE,
        sink
      }),
      { wrapper: StrictMode }
    );
    await settle();

    // Declared up on mounting, taken down by the simulated unmount, and put back up by the remount.
    expect(log.filter((entry) => entry === "sink" || entry === "raise")).toEqual(["raise", "sink", "raise"]);
  });
});

describe("reconciling with the host", () => {
  /**
   * A pane mounting onto a page a previous pane took down on its way out. Nothing else would ever
   * raise it, and the page would simply be invisible: open, owned, navigating, and behind a
   * renderer drawing nothing in its place.
   */
  it("raises a page it finds already sunk, which nothing else would ever put back", async () => {
    const view = mount({ initial: { hostParked: true } });

    expect(view.mountLog).toEqual(["raise"]);
  });

  /**
   * Sleeping, suspending, hiding and re-presenting all restack the page without this hook asking,
   * and none of them is an event it can hear. Left believing the page is still down, the pane would
   * go on painting a still over a page that is live again.
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
    // The declaration made on mounting is answered; only what follows is under test.
    answered.shift()?.();
    await settle();
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
   * The animation frame a sink waits out is a window in which the hook has decided to sink,
   * has not asked yet, and the host truthfully still says the page is up. A reconcile that could
   * not see that window would fire inside it and sink the page early — before the `<img>` standing
   * in for it had been painted, which is the one ordering the whole mechanism exists to get right.
   */
  it("holds off while a transition has decided but not yet asked", async () => {
    const view = mount();
    await advance(FIRST_FRAME_MS);
    log.length = 0;
    view.rerender(props({ parked: true, covered: true }));
    await settle();
    expect(log).toEqual(["publish"]);

    // A status poll lands mid-transition, reporting exactly what is still true.
    view.rerender(props({ parked: true, covered: true, hostParked: false, hostCovered: false }));
    await settle();
    expect(log).toEqual(["publish"]);

    drainFrame();
    await settle();
    expect(log).toEqual(["publish", "sink", "cover"]);
  });

  /**
   * The drift that begins inside the very window nothing may be sent in.
   *
   * A host that drops a sink while a request is still unanswered is the worst case of all: the
   * pane is painting a still over a page that is live again, and the only thing that could
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
    answered.shift()?.();
    await settle();
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
