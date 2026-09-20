import type { RefObject } from "react";
import { useEffect, useRef, useState } from "react";

/**
 * Whether the user is using the browser page right now, and the live page therefore belongs on
 * top of the app rather than a projection of it.
 *
 * The page is a native child window, so this cannot be written the obvious way. The moment the
 * page is raised it takes every pointer event over its own rectangle, which means the renderer
 * sees the pointer *leave* the page area at exactly the instant it arrives: a `pointerleave` on
 * the page element fires as a consequence of engaging, not of disengaging, and using it would
 * park the page immediately and forever.
 *
 * So leaving is only ever inferred from positive evidence of being somewhere else — a pointer
 * event the renderer actually received, at coordinates outside the page box. The renderer is only
 * sent those where the page is not covering, which makes them proof rather than a guess.
 */

/** Whether a viewport point falls inside `rect`. */
export function pointerIsInsideRect(
  x: number,
  y: number,
  rect: { left: number; top: number; right: number; bottom: number; width: number; height: number } | null
): boolean {
  if (!rect || !(rect.width > 0) || !(rect.height > 0)) return false;
  return x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom;
}

export type BrowserPageEngagementOptions = {
  /** The page box the native window is positioned from. */
  pageAreaRef: RefObject<HTMLElement | null>;
  /**
   * Whether there is a live page in this pane worth raising at all.
   *
   * False while the pane is showing one of its own cards, while it is not the active pane, and in
   * every runtime where the page is not a native child window. Engagement is meaningless in all
   * three and would only cost round trips.
   */
  enabled: boolean;
};

export function useBrowserPageEngagement({
  pageAreaRef,
  enabled
}: BrowserPageEngagementOptions): boolean {
  const [engaged, setEngaged] = useState(false);
  const engagedRef = useRef(false);
  /**
   * Whether the page is believed to hold the keyboard.
   *
   * Once the user has clicked into the page they may be typing in it, and the pointer wandering
   * off is no longer a statement that they are done: parking then would leave them typing into
   * something they cannot see. The renderer cannot ask the page whether it has focus, but it can
   * see its own webview lose focus while the page was the thing on top, which in a single window
   * means the sibling took it.
   */
  const pageHoldsKeyboardRef = useRef(false);

  useEffect(() => {
    engagedRef.current = false;
    pageHoldsKeyboardRef.current = false;
    setEngaged(false);
    if (!enabled) return;

    let frame = 0;
    const apply = (next: boolean) => {
      if (engagedRef.current === next) return;
      engagedRef.current = next;
      setEngaged(next);
    };
    const evaluate = (x: number, y: number) => {
      if (pointerIsInsideRect(x, y, pageAreaRef.current?.getBoundingClientRect() ?? null)) {
        apply(true);
        return;
      }
      // Outside the page. The keyboard is the only thing left that can keep it up.
      apply(pageHoldsKeyboardRef.current);
    };

    const onPointerImmediate = (event: PointerEvent) => {
      // A press outside the page moves focus into the app, so whatever the page was holding it is
      // about to lose — decided before the evaluation below reads it.
      if (
        event.type === "pointerdown"
        && !pointerIsInsideRect(
          event.clientX,
          event.clientY,
          pageAreaRef.current?.getBoundingClientRect() ?? null
        )
      ) {
        pageHoldsKeyboardRef.current = false;
      }
      window.cancelAnimationFrame(frame);
      frame = 0;
      evaluate(event.clientX, event.clientY);
    };

    // Coalesced to one layout read per frame. A pointer crossing the window produces events far
    // faster than the page can be restacked, and each one would otherwise force a reflow.
    const onPointerMove = (event: PointerEvent) => {
      if (frame) return;
      const { clientX, clientY } = event;
      frame = window.requestAnimationFrame(() => {
        frame = 0;
        evaluate(clientX, clientY);
      });
    };

    const onBlur = () => {
      // Focus left this webview while the page was the surface on top: in a single window that is
      // the page's own child window taking it.
      if (engagedRef.current) pageHoldsKeyboardRef.current = true;
    };
    const onFocus = () => {
      // This webview has the keyboard, so the page does not.
      pageHoldsKeyboardRef.current = false;
    };
    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") return;
      pageHoldsKeyboardRef.current = false;
      apply(false);
    };

    // Capture phase: a page area that stops events from bubbling must not be able to make the
    // pane forget where the pointer is.
    window.addEventListener("pointermove", onPointerMove, true);
    window.addEventListener("pointerover", onPointerImmediate, true);
    window.addEventListener("pointerdown", onPointerImmediate, true);
    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("pointermove", onPointerMove, true);
      window.removeEventListener("pointerover", onPointerImmediate, true);
      window.removeEventListener("pointerdown", onPointerImmediate, true);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [enabled, pageAreaRef]);

  return engaged;
}
