import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";

export type DragPoint = { x: number; y: number };

export type ReorderDropTarget = {
  id: string;
  position: "before" | "after";
};

type PointerSession<Item> = {
  item: Item;
  pointerId: number;
  startX: number;
  startY: number;
  element: HTMLElement;
  dragging: boolean;
};

const dragExclusionSelector = "[data-drag-exclude], input, textarea, select, option, a[href]";

export function reorderItems<T>(
  items: T[],
  sourceId: string,
  targetId: string,
  position: "before" | "after",
  getId: (item: T) => string
): T[] {
  if (sourceId === targetId) return items;
  const sourceIndex = items.findIndex((item) => getId(item) === sourceId);
  const targetIndex = items.findIndex((item) => getId(item) === targetId);
  if (sourceIndex < 0 || targetIndex < 0) return items;
  const next = [...items];
  const [moving] = next.splice(sourceIndex, 1);
  const adjustedTargetIndex = next.findIndex((item) => getId(item) === targetId);
  next.splice(adjustedTargetIndex + (position === "after" ? 1 : 0), 0, moving);
  return next;
}

function visibleRect(element: HTMLElement): DOMRect | null {
  const rect = element.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0 ? rect : null;
}

/** Find the nearest row in a sortable list without relying on the pointer event target.
 * This works for both pointer capture and the window-level fallback listeners.
 */
export function findReorderDropTarget(
  listId: string,
  sourceId: string,
  point: DragPoint
): ReorderDropTarget | null {
  const list = Array.from(document.querySelectorAll<HTMLElement>("[data-sortable-list]"))
    .find((candidate) => candidate.dataset.sortableList === listId);
  if (!list) return null;
  const listRect = visibleRect(list);
  if (!listRect || point.x < listRect.left - 24 || point.x > listRect.right + 24 || point.y < listRect.top - 24 || point.y > listRect.bottom + 24) {
    return null;
  }

  const candidates = Array.from(list.querySelectorAll<HTMLElement>("[data-sortable-id]"))
    .filter((element) => element.dataset.sortableId !== sourceId)
    .map((element) => ({ element, rect: visibleRect(element) }))
    .filter((candidate): candidate is { element: HTMLElement; rect: DOMRect } => candidate.rect !== null);
  if (!candidates.length) return null;

  const style = window.getComputedStyle(list);
  const horizontal = style.display.includes("flex") && style.flexDirection.startsWith("row");
  const coordinate = horizontal ? point.x : point.y;
  const nearest = candidates.reduce((best, candidate) => {
    const center = horizontal
      ? candidate.rect.left + candidate.rect.width / 2
      : candidate.rect.top + candidate.rect.height / 2;
    const distance = Math.abs(coordinate - center);
    return distance < best.distance ? { candidate, distance, center } : best;
  }, {
    candidate: candidates[0],
    distance: Number.POSITIVE_INFINITY,
    center: horizontal
      ? candidates[0].rect.left + candidates[0].rect.width / 2
      : candidates[0].rect.top + candidates[0].rect.height / 2
  });

  return {
    id: nearest.candidate.element.dataset.sortableId ?? "",
    position: coordinate < nearest.center ? "before" : "after"
  };
}

export function usePointerDrag<Item, Target>({
  getTarget,
  onDrop,
  threshold = 5
}: {
  getTarget: (point: DragPoint, item: Item) => Target | null;
  onDrop: (item: Item, target: Target) => void;
  threshold?: number;
}) {
  const [activeItem, setActiveItem] = useState<Item | null>(null);
  const [dropTarget, setDropTarget] = useState<Target | null>(null);
  const sessionRef = useRef<PointerSession<Item> | null>(null);
  const clickBlockerRef = useRef<{ handler: (event: MouseEvent) => void; timeout: number } | null>(null);
  const getTargetRef = useRef(getTarget);
  const onDropRef = useRef(onDrop);
  getTargetRef.current = getTarget;
  onDropRef.current = onDrop;

  const removeClickBlocker = useCallback(() => {
    const blocker = clickBlockerRef.current;
    if (!blocker) return;
    window.removeEventListener("click", blocker.handler, true);
    window.clearTimeout(blocker.timeout);
    clickBlockerRef.current = null;
  }, []);

  const suppressNextClick = useCallback(() => {
    removeClickBlocker();
    const handler = (event: MouseEvent) => {
      event.preventDefault();
      event.stopPropagation();
      event.stopImmediatePropagation();
      removeClickBlocker();
    };
    window.addEventListener("click", handler, true);
    clickBlockerRef.current = {
      handler,
      timeout: window.setTimeout(removeClickBlocker, 350)
    };
  }, [removeClickBlocker]);

  const clear = useCallback(() => {
    const session = sessionRef.current;
    sessionRef.current = null;
    if (session) {
      try {
        if (session.element.hasPointerCapture?.(session.pointerId)) {
          session.element.releasePointerCapture(session.pointerId);
        }
      } catch {
        // The browser can release capture before pointercancel/lostpointercapture arrives.
      }
    }
    setActiveItem(null);
    setDropTarget(null);
    document.body.classList.remove("pointer-sort-active");
  }, []);

  const cancel = useCallback(() => {
    if (sessionRef.current?.dragging) suppressNextClick();
    clear();
  }, [clear, suppressNextClick]);

  useEffect(() => {
    if (activeItem === null) return;
    const cancelWithEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      cancel();
    };
    window.addEventListener("keydown", cancelWithEscape);
    return () => window.removeEventListener("keydown", cancelWithEscape);
  }, [activeItem, cancel]);

  const move = useCallback((event: PointerEvent) => {
    const session = sessionRef.current;
    if (!session || event.pointerId !== session.pointerId) return;
    if (!session.dragging && Math.hypot(event.clientX - session.startX, event.clientY - session.startY) < threshold) return;
    if (!session.dragging) {
      session.dragging = true;
      try {
        session.element.setPointerCapture?.(event.pointerId);
      } catch {
        // Window-level listeners keep the drag alive when a WebView rejects capture.
      }
      setActiveItem(session.item);
      document.body.classList.add("pointer-sort-active");
    }
    if (event.cancelable) event.preventDefault();
    const target = getTargetRef.current({ x: event.clientX, y: event.clientY }, session.item);
    setDropTarget(target);
  }, [threshold]);

  const finish = useCallback((event: PointerEvent, cancelled: boolean) => {
    const session = sessionRef.current;
    if (!session || event.pointerId !== session.pointerId) return;
    const moved = Math.hypot(event.clientX - session.startX, event.clientY - session.startY) >= threshold;
    if (!session.dragging && moved) session.dragging = true;
    if (session.dragging) {
      const target = cancelled ? null : getTargetRef.current({ x: event.clientX, y: event.clientY }, session.item);
      event.preventDefault();
      suppressNextClick();
      if (!cancelled && target !== null) onDropRef.current(session.item, target);
    }
    clear();
  }, [clear, suppressNextClick, threshold]);

  useEffect(() => {
    const onPointerMove = (event: PointerEvent) => move(event);
    const onPointerUp = (event: PointerEvent) => finish(event, false);
    const onPointerCancel = (event: PointerEvent) => finish(event, true);
    window.addEventListener("pointermove", onPointerMove, { capture: true, passive: false });
    window.addEventListener("pointerup", onPointerUp, { capture: true, passive: false });
    window.addEventListener("pointercancel", onPointerCancel, { capture: true, passive: false });
    return () => {
      window.removeEventListener("pointermove", onPointerMove, true);
      window.removeEventListener("pointerup", onPointerUp, true);
      window.removeEventListener("pointercancel", onPointerCancel, true);
    };
  }, [finish, move]);

  useEffect(() => () => {
    document.body.classList.remove("pointer-sort-active");
    removeClickBlocker();
  }, [removeClickBlocker]);

  const bind = (item: Item) => ({
    onPointerDown: (event: ReactPointerEvent<HTMLElement>) => {
      if (event.button !== 0 || event.isPrimary === false) return;
      const target = event.target as Element;
      if (target.closest(dragExclusionSelector)) return;
      const element = event.currentTarget;
      sessionRef.current = {
        item,
        pointerId: event.pointerId,
        startX: event.clientX,
        startY: event.clientY,
        element,
        dragging: false
      };
    }
  });

  return { activeItem, dropTarget, bind, cancel };
}
