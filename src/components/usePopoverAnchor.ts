import type { RefObject } from "react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useFloatingSurface } from "../lib/floatingSurfaces";

const VIEWPORT_MARGIN = 8;
const ANCHOR_GAP = 6;

export interface PopoverPosition {
  left: number;
  top: number;
  /** Panel opens above the trigger; trigger-adjacent elements can move to the opposite edge. */
  flipped: boolean;
  minWidth: number;
}

export interface PopoverAnchorOptions {
  /** Alignment edge between panel and trigger. */
  align?: "start" | "end";
  /** Fixed panel width in px, used only before the actual width can be measured. */
  width?: number;
  /**
   * Place the panel's top-left corner at the pointer instead of below the trigger.
   * Keyboard activation reports no coordinates and falls back to the trigger.
   */
  anchorToPointer?: boolean;
  /** Invoked once each time the popover opens, for lazy list loading. */
  onOpen?: () => void;
  /**
   * Opens the panel whenever this value changes. For callers that need to hand
   * the user straight to a menu — a dialog that closes onto the control it was
   * asking about. The first value only establishes the baseline.
   */
  openSignal?: number;
}

export interface PopoverAnchor<
  Trigger extends HTMLElement = HTMLButtonElement,
  Panel extends HTMLElement = HTMLDivElement
> {
  open: boolean;
  /** Null before measurement; render the panel one frame with `visibility: hidden`. */
  position: PopoverPosition | null;
  triggerRef: RefObject<Trigger | null>;
  panelRef: RefObject<Panel | null>;
  toggle: (event?: { clientX: number; clientY: number; detail: number }) => void;
  close: (refocus: boolean) => void;
}

/**
 * Shared popover positioning and dismissal for `PopoverMenu` and `ContextUsageMeter`.
 *
 * Callers must portal panels to `document.body`: `.collapse-region__inner` has a persistent
 * transform and `overflow: hidden`, so a local fixed-position panel uses that ancestor as its
 * containing block and is clipped. Re-measure after every render because callers create fresh
 * React nodes each frame; the equality guard in `setPosition` prevents a render loop. Invoke
 * `onOpen` in an effect rather than a state updater so StrictMode cannot duplicate its side effect.
 */
export function usePopoverAnchor<
  Trigger extends HTMLElement = HTMLButtonElement,
  Panel extends HTMLElement = HTMLDivElement
>({ align = "start", width, anchorToPointer = false, onOpen, openSignal }: PopoverAnchorOptions = {}): PopoverAnchor<Trigger, Panel> {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<PopoverPosition | null>(null);
  const triggerRef = useRef<Trigger>(null);
  const panelRef = useRef<Panel>(null);
  const pointerRef = useRef<{ x: number; y: number } | null>(null);
  const openSignalRef = useRef(openSignal);
  const onOpenRef = useRef(onOpen);
  onOpenRef.current = onOpen;

  useEffect(() => {
    if (openSignal === undefined || openSignal === openSignalRef.current) return;
    openSignalRef.current = openSignal;
    setOpen(true);
  }, [openSignal]);

  const close = useCallback((refocus: boolean) => {
    setOpen(false);
    if (refocus) window.requestAnimationFrame(() => triggerRef.current?.focus());
  }, []);

  const toggle = useCallback((event?: { clientX: number; clientY: number; detail: number }) => {
    // `detail` is 0 for keyboard activation, whose coordinates are meaningless.
    pointerRef.current = event && event.detail > 0
      ? { x: event.clientX, y: event.clientY }
      : null;
    setOpen((current) => !current);
  }, []);

  useEffect(() => {
    if (!open) {
      setPosition(null);
      return;
    }
    onOpenRef.current?.();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (triggerRef.current?.contains(target) || panelRef.current?.contains(target)) return;
      setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      close(true);
    };
    // Scroll or zoom invalidates measured coordinates. Close transient panels rather than leaving
    // them at a stale position. The scroll listener captures, so it also sees scrolls from the
    // panel's own list; those move nothing the position depends on, and closing on them makes a
    // long menu impossible to scroll.
    const onViewportChange = (event: Event) => {
      const target = event.target;
      if (target instanceof Node && panelRef.current?.contains(target)) return;
      setOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    document.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("resize", onViewportChange);
    window.addEventListener("scroll", onViewportChange, true);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("resize", onViewportChange);
      window.removeEventListener("scroll", onViewportChange, true);
    };
  }, [close, open]);

  useLayoutEffect(() => {
    if (!open) return;
    const anchor = triggerRef.current?.getBoundingClientRect();
    const panel = panelRef.current?.getBoundingClientRect();
    if (!anchor || !panel) return;
    const viewportWidth = window.innerWidth || panel.width;
    const viewportHeight = window.innerHeight || panel.height;
    const panelWidth = panel.width || width || anchor.width;
    const panelHeight = panel.height;
    const pointer = anchorToPointer ? pointerRef.current : null;

    const preferredLeft = pointer
      ? pointer.x
      : align === "end" ? anchor.right - panelWidth : anchor.left;
    const maxLeft = Math.max(VIEWPORT_MARGIN, viewportWidth - panelWidth - VIEWPORT_MARGIN);
    const left = Math.min(Math.max(preferredLeft, VIEWPORT_MARGIN), maxLeft);

    const below = pointer ? pointer.y : anchor.bottom + ANCHOR_GAP;
    const roomBelow = viewportHeight - below - VIEWPORT_MARGIN;
    const roomAbove = (pointer ? pointer.y : anchor.top) - ANCHOR_GAP - VIEWPORT_MARGIN;
    const flipped = panelHeight > roomBelow && roomAbove > roomBelow;
    const top = flipped
      ? Math.max(VIEWPORT_MARGIN, (pointer ? pointer.y : anchor.top) - ANCHOR_GAP - panelHeight)
      : Math.min(below, Math.max(VIEWPORT_MARGIN, viewportHeight - panelHeight - VIEWPORT_MARGIN));

    setPosition((current) => {
      const next = { left, top, flipped, minWidth: anchor.width };
      if (
        current
        && current.left === next.left
        && current.top === next.top
        && current.flipped === next.flipped
        && current.minWidth === next.minWidth
      ) return current;
      return next;
    });
  });

  // Declared after the placement pass so the box published in a commit is the placed one, and
  // gated on `position` so the first, unplaced frame publishes nothing. Every popover in the app
  // reaches the built-in browser's native page through this one call.
  useFloatingSurface(panelRef, open && position !== null);

  return { open, position, triggerRef, panelRef, toggle, close };
}
