import type { RefObject } from "react";
import { useCallback, useEffect, useLayoutEffect, useRef } from "react";

/**
 * The picture of the native page the pane paints in the page's own place while the page is down.
 *
 * The built-in browser's page is a native child window, and a native child window paints above
 * every HTML layer no matter what the stacking context says. The page is nonetheless left on top
 * whenever nothing needs it gone: that is the only way the user sees it live, at full frame rate,
 * while the Agent drives it. It goes under the renderer only for as long as something has to be
 * seen in its place — a menu or dialog drawn across it, a pane covering it, or no page to show —
 * and the pane paints a capture of it there meanwhile.
 *
 * Two orderings are the whole mechanism, and they are not symmetric:
 *
 * - Sinking: **publish a frame in the same commit, wait one frame, then sink.** The transition
 *   runs as a layout effect, so the `<img>` is painted together with whatever caused the sink;
 *   one animation frame later it is on screen, and only then does the page leave. A native window
 *   cannot be cross-faded with HTML.
 * - Raising: **raise, await the host, wait one frame, then drop the `<img>`.** Dropping it first
 *   would show the pane's background until the page came back.
 *
 * Every failure path still sinks. A page that stays on top of an open dialog is the one outcome
 * that has no recovery: the dialog is unreachable and unreadable. A frozen still, or an empty box
 * where the still failed, costs a moment of staleness and nothing else.
 */
export type BrowserPageSnapshot = {
  /** The session the frame was captured from; a frame from another session is a lie, not a stale image. */
  contentKey: string;
  /** `data:image/png;base64,…`. */
  src: string;
  /** The base64 payload alone, kept so an unchanged capture can be recognised without re-decoding. */
  data: string;
  /** CSS pixels measured from the placeholder, not the PNG's natural size, which is device pixels. */
  width: number;
  height: number;
};

/**
 * A one-value store the frame is published through.
 *
 * The panel that owns the page also owns its toolbar, menus and drawer. Publishing through React
 * state would re-render all of that for every frame; a store lets the `<img>` be the only
 * subscriber.
 */
export type BrowserSnapshotStore = {
  set: (snapshot: BrowserPageSnapshot | null) => void;
  get: () => BrowserPageSnapshot | null;
  subscribe: (listener: () => void) => () => void;
};

export function createBrowserSnapshotStore(): BrowserSnapshotStore {
  let snapshot: BrowserPageSnapshot | null = null;
  const listeners = new Set<() => void>();
  return {
    set: (next) => {
      if (snapshot === next) return;
      snapshot = next;
      for (const listener of Array.from(listeners)) listener();
    },
    get: () => snapshot,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    }
  };
}

/** A capture that came back one pixel wide is what an unpainted surface returns, not a frame. */
const MIN_USABLE_CAPTURE_PX = 1;

/**
 * How often a raised page is captured to keep a frame warm for the next cover.
 *
 * One screenshot per second per presented page, on the same WebView2 UI thread the Agent's
 * automation runs on. A menu that had to wait for a capture round trip would open behind the
 * page, so the frame has to exist before anything asks for it.
 */
const FRAME_INTERVAL_MS = 1_000;

/** How soon after the page comes up the first warm frame is taken. */
const FIRST_FRAME_DELAY_MS = 300;

/**
 * Cadence for a page that has stopped changing.
 *
 * A preview nobody is touching produces byte-identical captures indefinitely. Recognising that
 * costs a string compare and takes an idle pane from one capture a second to one every five.
 */
const IDLE_FRAME_INTERVAL_MS = 5_000;
const IDENTICAL_FRAMES_BEFORE_IDLE = 5;

/** Backoff after captures that produce nothing, so a suspended page is not asked every second. */
const FRAME_BACKOFF_MS = 4_000;
const FAILURES_BEFORE_BACKOFF = 4;

export type BrowserPageProjectionOptions = {
  /** The session the page belongs to. A change discards the frame rather than re-capturing it. */
  contentKey: string | null;
  /**
   * Whether there is a page session to speak for — not whether its page exists yet.
   *
   * The pane declares where its page belongs as soon as it mounts, before the page is created or
   * presented, and the host keeps that declaration through presentation. A pane that waited for
   * the page to exist would have it presented on top of the start card it should sit beneath, as
   * a blank rectangle, until the declaration caught up.
   */
  enabled: boolean;
  /**
   * Whether the host is showing the page in the pane right now (`BrowserStatus.open`).
   *
   * Only a presented page is captured. A capture of a page that is not presented moves it off
   * screen and back, which is wasted on a page nobody sees and, if the page is presented while it
   * runs, ends by putting it back where it was hidden.
   */
  presented: boolean;
  /**
   * Whether the pane is presenting the page rather than one of its own cards.
   *
   * A pane showing the start page has a native `about:blank` underneath it that is worth neither
   * capturing nor painting: the card above is opaque and full-bleed. The page still has to be
   * sunk — `parked` says so — but nothing is held for it.
   */
  hasContent: boolean;
  /** Whether the page belongs beneath the renderer right now. */
  parked: boolean;
  /**
   * Whether a trusted surface is drawn over the page, as opposed to the page merely being out of
   * sight (its pane covered by another pane, or showing a card of its own).
   *
   * The host is told, because covering takes the page out of agent automation: the user is
   * looking at a dialog, and an Agent click landing behind it would be a click nobody could see.
   * A page that is merely out of sight stays the Agent's.
   */
  covered: boolean;
  /** The box the host positions the page from; the frame is measured and painted against it. */
  placeholderRef: RefObject<HTMLElement | null>;
  /**
   * What the host says it has done, polled from `BrowserStatus`.
   *
   * Sleeping, suspending, hiding and re-presenting all restack the page without this hook asking,
   * and none of them is an event the pane can hear. This is the only channel that says so, and it
   * is what lets a pane that mounts onto an already sunk page put it back rather than leaving an
   * invisible page behind.
   */
  hostParked: boolean;
  hostCovered: boolean;
  /** Sinks the native page beneath the renderer (`true`) or raises it (`false`). */
  setParked: (parked: boolean) => Promise<unknown>;
  /** Tells the host whether the sink is a cover or a rest. */
  setCovered: (covered: boolean) => Promise<unknown>;
  /** The page as a base64 PNG, or `null` when there is nothing to capture. */
  capture: () => Promise<{ data: string } | null>;
  /** Where the frame is published. */
  sink: BrowserSnapshotStore;
};

/**
 * Keeps the native page on top except while `parked` says otherwise, with a still in its place
 * for as long as it is down.
 *
 * Returns nothing: the frame goes to `sink` and the stacking goes to the host, so the caller only
 * has to render the `<img>` and hand this hook the same element it publishes as the page box.
 */
export function useBrowserPageProjection({
  contentKey,
  enabled,
  presented,
  hasContent,
  parked,
  covered,
  placeholderRef,
  hostParked,
  hostCovered,
  setParked,
  setCovered,
  capture,
  sink
}: BrowserPageProjectionOptions): void {
  // Read through a ref so a caller that rebuilds these callbacks every render — which every
  // caller does, since they close over the session id — does not re-run the transition effect and
  // re-capture the page on an unrelated render.
  const callbacks = useRef({ setParked, setCovered, capture, sink });
  useLayoutEffect(() => {
    callbacks.current = { setParked, setCovered, capture, sink };
  }, [setParked, setCovered, capture, sink]);

  // The last states the host was asked for, not the last ones React rendered. The transitions are
  // driven off these so a re-run that did not actually change anything does nothing. `null` is
  // "never said": whatever the host holds was declared by some earlier pane, so the first decision
  // is always sent, whichever way it goes.
  const parkedRef = useRef<boolean | null>(null);
  const coveredRef = useRef<boolean | null>(null);
  // How many host requests are still unanswered. The host's reported state disagrees with ours for
  // the whole width of a request, so the reconcile below has to know to keep out of that window
  // rather than re-asking on every 700ms poll for as long as the answer takes.
  const inFlightRef = useRef(0);
  /**
   * The host's last reported stacking, and whether a repair is owed on it.
   *
   * Disagreement alone is not a reason to act: between asking for something and the host applying
   * it every poll answers with the old value, and so does every poll while the host is refusing
   * outright — which is the state the pane mounts into while the page is still being created.
   * Acting on the standing disagreement would turn a page the host has not got round to into a
   * request storm against it.
   *
   * So only a *change* in what the host reports arms a repair. And once armed it stays armed until
   * it is made, because the change that armed it may well land inside a window where nothing may be
   * sent — which is exactly when the host and the pane drift furthest apart.
   */
  const lastHostParkedRef = useRef(false);
  const lastHostCoveredRef = useRef(false);
  const repairOwedRef = useRef(false);
  // Whether a transition has decided what to ask for but has not asked yet. Sinking deliberately
  // waits an animation frame between publishing the frame and taking the page away, and for that
  // whole window the refs say "sunk" while nothing is in flight and the host still says "raised".
  // A reconcile that could not see this window would fire inside it and sink the page early —
  // before the `<img>` standing in for it had been painted, which is the one ordering the whole
  // mechanism exists to get right.
  //
  // A token rather than a boolean: a superseded transition still runs its own tail (a queued
  // animation frame, a capture that lands late) and must not clear a guard a newer one is holding.
  const settlingRef = useRef(0);
  const transitionRef = useRef(0);

  // `enabled` is folded in here rather than handled separately so that losing the page cancels an
  // in-flight sink through the same cleanup path. Raising from its own effect would race that
  // capture, and the capture's failure path — which always sinks — would win, leaving the page
  // stacked under a renderer that is no longer painting anything in its place.
  const sunk = enabled && parked;
  const covering = enabled && parked && covered;

  const request = useCallback((
    which: "setParked" | "setCovered",
    value: boolean
  ): Promise<void> => {
    inFlightRef.current += 1;
    return Promise.resolve(callbacks.current[which](value))
      .catch(() => undefined)
      .finally(() => {
        inFlightRef.current -= 1;
      })
      .then(() => undefined);
  }, []);

  /**
   * The newest frame, held whether or not it is currently on screen.
   *
   * While the page is raised this is what makes covering instant: capturing only once a surface
   * has already opened means the page goes on painting over that surface for the whole round trip
   * — a screenshot crosses CDP, the single WebView2 UI thread and Tauri IPC — and a menu that is
   * invisible for several frames after the click is the very thing this mechanism prevents.
   */
  const warmFrameRef = useRef<BrowserPageSnapshot | null>(null);

  /** Captures, decodes and validates one frame, or `null` if there is nothing usable to show. */
  const grabFrame = useCallback(async (
    key: string,
    width: number,
    height: number
  ): Promise<BrowserPageSnapshot | null> => {
    if (!(width > 0) || !(height > 0)) return null;
    const capture = await callbacks.current.capture();
    if (!capture?.data) return null;
    const src = `data:image/png;base64,${capture.data}`;
    // Decoded before it is handed on so the page can never be taken away while the browser is
    // still turning bytes into pixels: an `<img>` that has not decoded paints nothing, and the
    // frames waited for after publishing would be spent on an empty box.
    const image = new Image();
    image.src = src;
    await image.decode().catch(() => undefined);
    // A surface that was not painting returns a single pixel rather than an error. Standing that
    // in for the page would read as a rendering bug; no frame at all reads as "no frame".
    if (
      image.naturalWidth <= MIN_USABLE_CAPTURE_PX
      || image.naturalHeight <= MIN_USABLE_CAPTURE_PX
    ) {
      return null;
    }
    return { contentKey: key, src, data: capture.data, width, height };
  }, []);

  useEffect(() => {
    const warm = warmFrameRef.current;
    if (warm && warm.contentKey !== (contentKey ?? "")) warmFrameRef.current = null;
  }, [contentKey]);

  /**
   * The capture loop, which keeps a frame warm for as long as the page is raised.
   *
   * It does not run while the page is down. Covered, the frame is frozen behind a dialog nobody
   * can see past; out of sight, nobody is looking at all. Either way a capture a second would be
   * spent on pixels that are not on screen.
   */
  useEffect(() => {
    if (!enabled || !presented || !hasContent || sunk) return;
    let cancelled = false;
    let timer = 0;
    const key = contentKey ?? "";
    let identical = 0;
    let failures = 0;
    const tick = async () => {
      let delay = FRAME_INTERVAL_MS;
      try {
        const box = placeholderRef.current?.getBoundingClientRect();
        const frame = await grabFrame(key, box?.width ?? 0, box?.height ?? 0);
        if (cancelled) return;
        if (frame) {
          failures = 0;
          // A page nobody is touching answers with the same bytes forever. Recognising that is
          // what keeps an idle preview from costing a screenshot a second for as long as it is
          // open, and comparing the payload is cheaper than the capture that produced it.
          identical = frame.data === warmFrameRef.current?.data ? identical + 1 : 0;
          warmFrameRef.current = frame;
          if (identical >= IDENTICAL_FRAMES_BEFORE_IDLE) delay = IDLE_FRAME_INTERVAL_MS;
        } else {
          // A failed capture keeps the frame it had rather than dropping to nothing: an older
          // picture of this page is still a better stand-in than the pane's bare background.
          failures += 1;
          if (failures >= FAILURES_BEFORE_BACKOFF) delay = FRAME_BACKOFF_MS;
        }
      } catch {
        if (cancelled) return;
        // Capturing fails for ordinary reasons — the page is suspended, being created, or busy —
        // and none of them is worth retrying at full rate.
        failures += 1;
        if (failures >= FAILURES_BEFORE_BACKOFF) delay = FRAME_BACKOFF_MS;
      }
      if (cancelled) return;
      timer = window.setTimeout(() => void tick(), delay);
    };
    // The first frame comes sooner than the cadence: a page just raised may be covered again at
    // any moment, and whatever it showed before it went down may have changed behind the cover.
    timer = window.setTimeout(() => void tick(), FIRST_FRAME_DELAY_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [enabled, presented, hasContent, sunk, contentKey, placeholderRef, grabFrame]);

  // A frame is a picture of one session's page. Switching sessions cannot be repaired by
  // re-measuring the way geometry can, so a frame left over from the outgoing session is dropped
  // outright rather than shown under the incoming one's chrome.
  useEffect(() => {
    const frames = callbacks.current.sink;
    const stale = frames.get();
    if (stale && stale.contentKey !== (contentKey ?? "")) frames.set(null);
  }, [contentKey]);

  /**
   * Cancels the transition in progress, if any.
   *
   * Held in a ref rather than returned as the effect's cleanup: the effect re-runs on changes that
   * are not transitions — a new session key, a placeholder — and a cleanup would cancel a sink
   * still waiting on its frame without anything taking its place, leaving the page on top of
   * whatever it was going down for. Only a newer transition, or the pane going away, cancels one.
   */
  const cancelTransitionRef = useRef<(() => void) | null>(null);
  const enabledRef = useRef(enabled);
  useLayoutEffect(() => {
    enabledRef.current = enabled;
  }, [enabled]);

  /**
   * The one place stacking is changed, and the only place the two flags are ordered against the
   * frame.
   *
   * A layout effect, so that whatever made the page go down — a menu, a dialog, a pane expanded
   * over this one — and the still standing in for the page are painted in the same frame. The
   * page itself leaves one animation frame later, once that paint is on screen.
   *
   * `covered` gets no effect of its own because the host sinks the page for either flag: sending
   * it the moment a menu opened would take the page away before its stand-in had been painted,
   * which is exactly the ordering this effect exists to enforce. So it rides along — issued after
   * the sink and before the raise — and is sent on its own only while the page is already down,
   * where there is nothing left to order it against.
   */
  useLayoutEffect(() => {
    const wasSunk = parkedRef.current;
    const wasCovering = coveredRef.current;
    if (sunk === wasSunk && covering === wasCovering) return;
    // Nothing to speak for yet, and nothing said: the first decision waits for a session.
    if (!enabled && wasSunk === null) return;
    cancelTransitionRef.current?.();
    parkedRef.current = sunk;
    coveredRef.current = covering;
    const { sink: frames } = callbacks.current;
    let cancelled = false;
    const token = ++transitionRef.current;
    settlingRef.current = token;
    const settled = () => {
      if (settlingRef.current === token) settlingRef.current = 0;
    };
    cancelTransitionRef.current = () => {
      cancelled = true;
      settled();
    };

    // Already down and staying down: a surface opened over a page that was out of sight, or one of
    // two surfaces closed. The page does not move, so there is nothing to sequence — only the
    // host's understanding of why it is down changes.
    if (sunk && wasSunk) {
      void request("setCovered", covering).finally(settled);
      return;
    }

    if (!sunk) {
      // Raising. The cover is dropped first so the host's own union does not hold the page down
      // after it has been asked to come up, then the page comes back, and the frame is dropped a
      // frame later so the `<img>` is still there for the paint in which the page reappears.
      void (async () => {
        if (wasCovering) await request("setCovered", false);
        if (cancelled) return;
        await request("setParked", false);
        // No still is standing in — the pane's first word on a page it has only just mounted
        // over — so there is no frame to wait out before taking it down.
        if (cancelled || frames.get() === null) return;
        window.requestAnimationFrame(() => {
          if (cancelled) return;
          frames.set(null);
        });
      })().finally(settled);
      return;
    }

    // Sinking.
    const box = placeholderRef.current?.getBoundingClientRect();
    const width = box?.width ?? 0;
    const height = box?.height ?? 0;
    const key = contentKey ?? "";

    /**
     * Takes the page down. Reached from every path — after a frame has been published when one
     * was available, immediately when none was — because a page left on top of a dialog is the
     * one outcome with no recovery.
     */
    const sinkPage = () => {
      if (cancelled) {
        settled();
        return;
      }
      void (async () => {
        await request("setParked", true);
        // Re-checked rather than assumed: a superseded sink still owns this chain, and sending
        // the cover it decided on would put the page back into user control after a newer
        // transition had already handed it back.
        if (cancelled || !covering) return;
        await request("setCovered", true);
      })().finally(settled);
    };

    // Nothing of this page is worth showing — the pane is drawing one of its own cards over the
    // whole body. Sink it with no frame at all rather than publishing a picture of a blank
    // document underneath an opaque card.
    if (!hasContent) {
      frames.set(null);
      sinkPage();
      return;
    }

    const warm = warmFrameRef.current;
    const usableWarm = warm && warm.contentKey === key && width > 0 && height > 0
      ? { ...warm, width, height }
      : null;

    // Published from this layout effect, the still re-renders before the browser paints, so it
    // appears in the very frame whatever covered the page does. One animation frame later that
    // paint is on screen and the page can go.
    if (usableWarm) {
      frames.set(usableWarm);
      window.requestAnimationFrame(sinkPage);
      return;
    }

    // No frame yet — the page went down within moments of first coming up. Waiting for a capture
    // would leave it over whatever is covering it for the whole round trip, so it goes down at
    // once and the box is filled when the capture lands. A page not yet presented is not on
    // screen to be stood in for, and is not captured at all.
    sinkPage();
    if (!presented) return;
    void grabFrame(key, width, height)
      .then((frame) => {
        if (cancelled || !frame) return;
        warmFrameRef.current = frame;
        frames.set(frame);
      })
      .catch(() => undefined);
  }, [sunk, covering, enabled, presented, hasContent, contentKey, placeholderRef, request, grabFrame]);

  /**
   * A pane that goes away while its page is up takes the page down on its way out.
   *
   * The pane goes because the browser is being hidden, closed, switched to another tab or another
   * conversation, and the host parks the page on each of those paths — but only once the renderer
   * gets round to asking, which is after the frame in which the pane has already vanished. For
   * that frame the page would be painted over whatever took the pane's place. Sinking from the
   * unmount, before that frame is painted, closes the gap; it cannot race the host's hide the way
   * a raise did, because both of them put the page down.
   *
   * `parkedRef` follows, so a pane that only remounts (React's development double mount) sees the
   * page as down and raises it again. A pane that never declared anything leaves the page alone.
   */
  useLayoutEffect(() => () => {
    cancelTransitionRef.current?.();
    cancelTransitionRef.current = null;
    if (!enabledRef.current || parkedRef.current !== false) return;
    parkedRef.current = true;
    void request("setParked", true);
  }, [request]);

  // A frame outliving the content it was a picture of is the same lie as one outliving its
  // session: the pane has swapped to a card, and what is behind that card is no longer this.
  useEffect(() => {
    if (hasContent) return;
    callbacks.current.sink.set(null);
    warmFrameRef.current = null;
  }, [hasContent]);

  /**
   * Puts the host back in step whenever it and this hook have drifted apart.
   *
   * Both directions matter and neither is reachable from the transition effects, because in both
   * the hook's own view of the world did not change:
   *
   * - The host dropped a sink nobody asked it to drop — sleeping, suspending, hiding and
   *   re-presenting all restack the page on their own — and the pane is left painting a still
   *   over a page that is live again, with whatever it captured for stranded behind it.
   * - The host sank a page this pane had declared up — a capture or another pane's late request
   *   landing after the declaration. Nothing would ever raise it and the page would simply be
   *   invisible.
   *
   * Gated on nothing being in flight and no transition mid-way. The host's answer legitimately
   * lags a request by the width of an IPC round trip, and a status poll landing inside that window
   * reports the old value.
   *
   * Runs on every render rather than on a change of the two host flags, because the change that
   * arms a repair may land inside one of those windows: an effect that only woke on the host's
   * value changing would drop that repair for good and leave the host holding a stacking nobody
   * was going to correct. The arming is what keeps this from re-asking on every poll instead.
   */
  useEffect(() => {
    const hostChanged = (
      hostParked !== lastHostParkedRef.current
      || hostCovered !== lastHostCoveredRef.current
    );
    lastHostParkedRef.current = hostParked;
    lastHostCoveredRef.current = hostCovered;
    const declaredParked = parkedRef.current;
    const declaredCovered = coveredRef.current;
    // Nothing to repair towards until the pane has said something itself.
    if (!enabled || declaredParked === null || declaredCovered === null) return;
    const parkedDiffers = hostParked !== declaredParked;
    const coveredDiffers = hostCovered !== declaredCovered;
    if (!parkedDiffers && !coveredDiffers) {
      repairOwedRef.current = false;
      return;
    }
    if (hostChanged) repairOwedRef.current = true;
    if (!repairOwedRef.current) return;
    if (inFlightRef.current > 0 || settlingRef.current !== 0) return;
    repairOwedRef.current = false;
    if (parkedDiffers) void request("setParked", declaredParked);
    if (coveredDiffers) void request("setCovered", declaredCovered);
  });

  // The still goes with the pane: nothing is left to paint it.
  useEffect(() => () => {
    callbacks.current.sink.set(null);
  }, []);
}

/** Whether `rects` contains anything that overlaps `pageRect`. */
export function anyRectOverlaps(
  rects: readonly { left: number; top: number; right: number; bottom: number; width: number; height: number }[],
  pageRect: { left: number; top: number; right: number; bottom: number; width: number; height: number }
): boolean {
  if (!(pageRect.width > 0) || !(pageRect.height > 0)) return false;
  return rects.some((rect) => (
    rect.width > 0
    && rect.height > 0
    && rect.left < pageRect.right
    && rect.right > pageRect.left
    && rect.top < pageRect.bottom
    && rect.bottom > pageRect.top
  ));
}
