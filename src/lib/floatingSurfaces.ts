import type { RefObject } from "react";
import { useCallback, useEffect, useLayoutEffect, useRef } from "react";

/**
 * Registry of the renderer's floating surfaces — menus, popovers, dialogs and viewers that are
 * portaled to `document.body` and drawn above the ordinary layout.
 *
 * The built-in browser's page is a native child window, and a native child window paints above
 * every HTML layer no matter what the stacking context says. The page is up whenever nothing needs
 * it gone, so a menu opened across it would be painted over: the overlap has to be noticed and
 * the page put down under a still of itself (`browser_action` with `action: "occlude"`, which also
 * takes the page out of agent automation, plus `"project"` for the sink itself — see
 * `browser.rs`). Surfaces publish from a layout effect so the pane hears of them before the
 * commit that drew them is painted.
 *
 * What the consumer needs from this registry is only whether anything overlaps the page — the
 * boxes are kept because that overlap test needs them, not because anyone positions anything from
 * them. A surface far from the page must not freeze it for nothing, which is the whole of why
 * geometry is published at all.
 *
 * Publishing is a property of *being* a floating surface, not something each call site opts into:
 * an opt-in channel only protects the surfaces somebody remembered to wire, and every menu added
 * afterwards silently goes back to being covered. `useFloatingSurface` is therefore called from
 * the shared primitives — `usePopoverAnchor`, `Dialog`, the image viewer — so a new caller of any
 * of them is protected without knowing this file exists.
 */

/** A floating surface's viewport box, by value. */
export type FloatingSurfaceRect = Pick<
  DOMRect,
  "left" | "top" | "right" | "bottom" | "width" | "height"
>;

const surfaces = new Map<number, FloatingSurfaceRect>();
const listeners = new Set<() => void>();
let claimed = 0;
let snapshot: FloatingSurfaceRect[] = [];

function sameRect(a: FloatingSurfaceRect, b: FloatingSurfaceRect): boolean {
  return a.left === b.left
    && a.top === b.top
    && a.right === b.right
    && a.bottom === b.bottom
    && a.width === b.width
    && a.height === b.height;
}

/** A registry key for one surface, stable for as long as that surface's component lives. */
export function claimFloatingSurfaceId(): number {
  claimed += 1;
  return claimed;
}

/**
 * Records where a surface is, or withdraws it with `null`. Unchanged geometry notifies nobody:
 * publishers measure after every render, and a menu that merely re-rendered must not make the
 * page recompute whether it is covered.
 */
export function publishFloatingSurface(id: number, rect: FloatingSurfaceRect | null): void {
  if (rect === null) {
    if (!surfaces.delete(id)) return;
  } else {
    const current = surfaces.get(id);
    if (current && sameRect(current, rect)) return;
    surfaces.set(id, rect);
  }
  snapshot = Array.from(surfaces.values());
  for (const listener of Array.from(listeners)) listener();
}

/** Every floating surface currently on screen. The array identity changes only on a real change. */
export function floatingSurfaceRects(): FloatingSurfaceRect[] {
  return snapshot;
}

/** Calls `listener` whenever the set of floating surfaces or any of their boxes changes. */
export function subscribeFloatingSurfaces(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Drops every registration. For tests, which share this module across cases. */
export function resetFloatingSurfaces(): void {
  surfaces.clear();
  snapshot = [];
}

/**
 * The surfaces that actually overlap `pageRect`.
 *
 * Covering the page costs a capture, a decode and a frozen still, so a menu opened far from the
 * page must not pay for it. Only surfaces that genuinely overlap count as covering it.
 */
export function floatingSurfacesOver(
  rects: readonly FloatingSurfaceRect[],
  pageRect: FloatingSurfaceRect
): FloatingSurfaceRect[] {
  if (!(pageRect.width > 0) || !(pageRect.height > 0)) return [];
  return rects.filter((rect) => (
    rect.width > 0
    && rect.height > 0
    && rect.left < pageRect.right
    && rect.right > pageRect.left
    && rect.top < pageRect.bottom
    && rect.bottom > pageRect.top
  ));
}

/**
 * Keeps `ref`'s box in the registry for as long as `active` holds.
 *
 * Measured after every render because a popover's own placement pass is a render: the first frame
 * of an open has no position yet, and reporting then would claim the page is covered from a box
 * the panel has not been placed in. A `ResizeObserver` covers the size changes that arrive
 * without one.
 */
export function useFloatingSurface(
  ref: RefObject<HTMLElement | null>,
  active: boolean
): void {
  const idRef = useRef<number | null>(null);
  if (idRef.current === null) idRef.current = claimFloatingSurfaceId();
  const id = idRef.current;

  const measure = useCallback(() => {
    const element = active ? ref.current : null;
    if (!element) {
      publishFloatingSurface(id, null);
      return;
    }
    const box = element.getBoundingClientRect();
    publishFloatingSurface(id, box.width > 0 && box.height > 0
      ? {
        left: box.left,
        top: box.top,
        right: box.right,
        bottom: box.bottom,
        width: box.width,
        height: box.height
      }
      : null);
  }, [active, id, ref]);

  useLayoutEffect(measure);

  useEffect(() => {
    const element = active ? ref.current : null;
    if (!element) return;
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(element);
    window.addEventListener("resize", measure);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [active, measure, ref]);

  // A surface that unmounts while open — a dialog closing, a pane going away — must not leave its
  // rectangle behind, or the page stays frozen behind a still of itself with nothing drawn over it.
  useEffect(() => () => publishFloatingSurface(id, null), [id]);
}
