import type { RefObject } from "react";
import { useCallback, useEffect, useLayoutEffect, useRef } from "react";

/**
 * The picture of the native page the pane paints in the page's own place.
 *
 * The built-in browser's page is a native child window, and a native child window paints above
 * every HTML layer no matter what the stacking context says. That makes the page, not the app,
 * the top surface of the preview pane: menus, dialogs, the pane's own rounded corners and every
 * transition around it are painted over by a rectangle the renderer does not control.
 *
 * So the page is sunk beneath the renderer by default and the pane paints a capture of it
 * instead. What the user looks at is therefore ordinary HTML — clipped, animated and drawn over
 * like anything else — and the live page is raised only for as long as it is being used: the
 * pointer is inside it, or it holds the keyboard. Raising is what keeps this a browser rather
 * than a picture of one; scrolling, text selection, the IME and native right-click all belong to
 * the real page and none of them survives being simulated.
 *
 * Two orderings are the whole mechanism, and they are not symmetric:
 *
 * - Sinking: **publish a frame, wait two frames, then sink.** The `<img>` has to have been
 *   painted before the page leaves, or there is a blank frame in between. A native window cannot
 *   be cross-faded with HTML.
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
 * The frame now changes on a timer rather than only on a transition, and the panel that owns the
 * page also owns its toolbar, menus and drawer. Publishing through React state would re-render
 * all of that once a second; a store lets the `<img>` be the only subscriber.
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
 * How often the projection is refreshed while it is what the user is looking at.
 *
 * One screenshot per second per presented page, on the same WebView2 UI thread the Agent's
 * automation runs on. That is the price of the page not being the top surface of the app, and it
 * is the same price the previous occlusion-only mechanism paid to keep a frame warm — what
 * changed is which state is the resting one, not how much it costs.
 */
const FRAME_INTERVAL_MS = 1_000;

/**
 * Cadence for a page that has stopped changing.
 *
 * A preview nobody is touching produces byte-identical captures indefinitely. Recognising that
 * costs a string compare and takes the idle pane from one capture a second to one every five,
 * which matters because projecting is now the state the pane spends nearly all its time in.
 */
const IDLE_FRAME_INTERVAL_MS = 5_000;
const IDENTICAL_FRAMES_BEFORE_IDLE = 5;

/** Backoff after captures that produce nothing, so a suspended page is not asked every second. */
const FRAME_BACKOFF_MS = 4_000;
const FAILURES_BEFORE_BACKOFF = 4;

export type BrowserPageProjectionOptions = {
  /** The session the page belongs to. A change discards the frame rather than re-capturing it. */
  contentKey: string | null;
  /** Whether there is a live native page to sink at all. */
  enabled: boolean;
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
   * Whether a trusted surface is drawn over the page, as opposed to the pane merely resting.
   *
   * Two consequences. The host is told, because covering takes the page out of agent automation
   * and projecting must not. And the frame is taken from the warm capture rather than a fresh one
   * — a menu that waits for a screenshot round trip is a menu that opens behind the page, which is
   * the entire fault this mechanism exists to prevent.
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
 * Keeps the native page sunk except while it is being used, with a live projection in its place.
 *
 * Returns nothing: the frame goes to `sink` and the stacking goes to the host, so the caller only
 * has to render the `<img>` and hand this hook the same element it publishes as the page box.
 */
export function useBrowserPageProjection({
  contentKey,
  enabled,
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
  // driven off these so a re-run that did not actually change anything does nothing.
  const parkedRef = useRef(false);
  const coveredRef = useRef(false);
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
  // waits two animation frames between publishing the frame and taking the page away, and for
  // that whole window the refs say "sunk" while nothing is in flight and the host still says
  // "raised". A reconcile that could not see this window would fire inside it and sink the page
  // early — before the `<img>` standing in for it had been painted, which is the one ordering the
  // whole mechanism exists to get right.
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
   * The capture loop.
   *
   * It runs in two of the three states the page can be in, for two different reasons: while the
   * page is raised it keeps a frame ready for the next cover, and while the pane is projecting it
   * *is* the projection. It deliberately does not run while a surface is covering the page —
   * there the frame is frozen behind a dialog nobody can see past, and a capture a second would
   * be spent on pixels that are not on screen.
   */
  useEffect(() => {
    if (!enabled || !hasContent || covering) return;
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
          // Published only while the projection is what the user is looking at, and only when it
          // would actually change: a raised page is showing itself, and rewriting the `<img>`'s
          // source once a second with the same bytes is a decode and a paint for nothing on a
          // preview that is sitting still.
          if (sunk) {
            const shown = callbacks.current.sink.get();
            const changed = (
              shown?.data !== frame.data
              || shown?.width !== frame.width
              || shown?.height !== frame.height
            );
            if (changed) callbacks.current.sink.set(frame);
          }
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
    timer = window.setTimeout(() => void tick(), FRAME_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [enabled, hasContent, covering, sunk, contentKey, placeholderRef, grabFrame]);

  // A frame is a picture of one session's page. Switching sessions cannot be repaired by
  // re-measuring the way geometry can, so a frame left over from the outgoing session is dropped
  // outright rather than shown under the incoming one's chrome.
  useEffect(() => {
    const frames = callbacks.current.sink;
    const stale = frames.get();
    if (stale && stale.contentKey !== (contentKey ?? "")) frames.set(null);
  }, [contentKey]);

  /**
   * The one place stacking is changed, and the only place the two flags are ordered against the
   * frame.
   *
   * `covered` gets no effect of its own because the host sinks the page for either flag: sending
   * it the moment a menu opened would take the page away before its stand-in had been painted,
   * which is exactly the ordering this effect exists to enforce. So it rides along — issued after
   * the sink and before the raise — and is sent on its own only while the page is already down,
   * where there is nothing left to order it against.
   */
  useEffect(() => {
    const wasSunk = parkedRef.current;
    const wasCovering = coveredRef.current;
    if (sunk === wasSunk && covering === wasCovering) return;
    parkedRef.current = sunk;
    coveredRef.current = covering;
    const { sink: frames } = callbacks.current;
    let cancelled = false;
    const token = ++transitionRef.current;
    settlingRef.current = token;
    const settled = () => {
      if (settlingRef.current === token) settlingRef.current = 0;
    };

    // Already down and staying down: a surface opened over a projection, or closed off one. The
    // page does not move, so there is nothing to sequence — only the host's understanding of why
    // it is down changes.
    if (sunk && wasSunk) {
      void request("setCovered", covering).finally(settled);
      return () => {
        cancelled = true;
        settled();
      };
    }

    if (!sunk) {
      // Raising. The cover is dropped first so the host's own union does not hold the page down
      // after it has been asked to come up, then the page comes back, and the frame is dropped a
      // frame later so the `<img>` is still there for the paint in which the page reappears.
      void (async () => {
        if (wasCovering) await request("setCovered", false);
        if (cancelled) return;
        await request("setParked", false);
        if (cancelled) return;
        window.requestAnimationFrame(() => {
          if (cancelled) return;
          frames.set(null);
        });
      })().finally(settled);
      return () => {
        cancelled = true;
        settled();
      };
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

    /** Publishes `frame`, lets it paint, and only then takes the page away. */
    const standIn = (frame: BrowserPageSnapshot) => {
      if (cancelled) {
        settled();
        return;
      }
      frames.set(frame);
      // Two frames, not one: the frame that commits the `<img>` and the frame that paints it.
      // Sinking after only the first would take the page away before its stand-in was on screen.
      window.requestAnimationFrame(() => {
        window.requestAnimationFrame(sinkPage);
      });
    };

    // Nothing of this page is worth showing — the pane is drawing one of its own cards over the
    // whole body. Sink it with no frame at all rather than publishing a picture of a blank
    // document underneath an opaque card.
    if (!hasContent) {
      frames.set(null);
      sinkPage();
      return () => {
        cancelled = true;
        settled();
      };
    }

    const warm = warmFrameRef.current;
    const usableWarm = warm && warm.contentKey === key && width > 0 && height > 0
      ? { ...warm, width, height }
      : null;

    // A surface has opened over the page and is invisible until this lands, so the warm frame —
    // at most one refresh interval old, which is invisible on a page the user was not touching —
    // is what makes covering instant. Waiting for a fresh capture here is exactly what used to
    // leave menus behind the page.
    if (covering && usableWarm) {
      standIn(usableWarm);
      return () => {
        cancelled = true;
        settled();
      };
    }

    // Nothing is waiting on this one: the user simply moved the pointer off the page. There is
    // time to capture the page as it is right now, and there has to be — a frame from a second
    // ago would snap the projection back to a scroll position the user has already left.
    void grabFrame(key, width, height)
      .then((frame) => {
        if (cancelled) return;
        // The freshest frame there is, so it is also the one the next cover should stand in: left
        // out, the loop below would spend its first five ticks deciding whether the page had
        // changed against nothing at all, and an idle preview would never reach its idle cadence.
        if (frame) warmFrameRef.current = frame;
        const next = frame ?? usableWarm;
        if (next) standIn(next);
        else sinkPage();
      })
      .catch(sinkPage);

    return () => {
      cancelled = true;
      settled();
    };
  }, [sunk, covering, hasContent, contentKey, placeholderRef, request, grabFrame]);

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
   * - A pane mounted onto a page the host still has sunk, from a previous pane that went away
   *   while it was projecting. Nothing would ever raise it and the page would simply be invisible.
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
    if (!enabled) return;
    const parkedDiffers = hostParked !== parkedRef.current;
    const coveredDiffers = hostCovered !== coveredRef.current;
    if (!parkedDiffers && !coveredDiffers) {
      repairOwedRef.current = false;
      return;
    }
    if (hostChanged) repairOwedRef.current = true;
    if (!repairOwedRef.current) return;
    if (inFlightRef.current > 0 || settlingRef.current !== 0) return;
    repairOwedRef.current = false;
    if (parkedDiffers) void request("setParked", parkedRef.current);
    if (coveredDiffers) void request("setCovered", coveredRef.current);
  });

  // A pane that unmounts deliberately leaves the page's stacking alone. It is not this hook's to
  // decide: the pane goes away because the browser is being hidden or closed, and the host sinks
  // and hides it on that path already — a raise racing it from here is what left a live page
  // painted over the app after the pane was gone. A pane that merely remounts is put back by the
  // reconcile above, which is the one place that reads what the host actually has.
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
