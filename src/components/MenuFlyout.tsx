import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { HTMLAttributes, ReactNode } from "react";
import { createPortal } from "react-dom";
import { useFloatingSurface } from "../lib/floatingSurfaces";

/** How close a panel may come to the window's edge. */
const VIEWPORT_MARGIN = 8;

/** The panel a submenu's row lies in, whose edge the submenu opens against: a menu, or a submenu. */
const PARENT_PANEL = ".popover-menu__panel, .context-menu, .context-menu__submenu";

/**
 * Every panel one menu has open: the menu's own and each of its submenus'. A submenu is a panel
 * of its own on the page, not a box inside the panel it came from, so a press or a scroll in it is
 * not inside that panel as far as the page can tell; the menu asks this instead.
 */
export interface MenuSurfaces {
  add: (element: HTMLElement) => () => void;
  contains: (target: Node | null) => boolean;
}

export function createMenuSurfaces(): MenuSurfaces {
  const elements = new Set<HTMLElement>();
  return {
    add: (element) => {
      elements.add(element);
      return () => {
        elements.delete(element);
      };
    },
    contains: (target) => {
      if (!target) return false;
      for (const element of elements) if (element.contains(target)) return true;
      return false;
    }
  };
}

export const MenuSurfacesContext = createContext<MenuSurfaces | null>(null);

interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export interface FlyoutPlacement {
  /** The row the submenu belongs to. */
  row: Box;
  /** The panel the row lies in. */
  parent: Box;
  /** The parent's side borders: the submenu's edge lies over the parent's, as one rule. */
  parentBorder: { left: number; right: number };
  width: number;
  height: number;
  /** The submenu's own border and padding, so its first (or last) row lines up with its row. */
  inset: { top: number; bottom: number };
  /** Down from the row's top, or up from its foot for a row at the bottom of its panel. */
  hang: "down" | "up";
  viewport: { width: number; height: number };
}

/**
 * Where a submenu goes: beside its panel, its edge over the panel's own, and lined up with its row.
 * It opens to the right, or to the left when the right has no room; it moves up, or down, by
 * however much would leave the window.
 */
function placeFlyout({
  row, parent, parentBorder, width, height, inset, hang, viewport
}: FlyoutPlacement): { left: number; top: number } {
  const right = parent.right - parentBorder.right;
  const left = right + width > viewport.width - VIEWPORT_MARGIN
    ? Math.max(VIEWPORT_MARGIN, parent.left + parentBorder.left - width)
    : right;
  const preferredTop = hang === "down" ? row.top - inset.top : row.bottom + inset.bottom - height;
  const top = Math.max(VIEWPORT_MARGIN, Math.min(preferredTop, viewport.height - VIEWPORT_MARGIN - height));
  return { left, top };
}

const pixels = (value: string): number => Number.parseFloat(value) || 0;

export interface MenuFlyoutProps extends Omit<HTMLAttributes<HTMLDivElement>, "className" | "style" | "children"> {
  className: string;
  hang?: "down" | "up";
  /** Focus the submenu's first control once it is placed: it was opened from the keyboard. */
  autoFocus?: boolean;
  children: ReactNode;
}

/**
 * A submenu's panel, beside the row that opened it.
 *
 * It is a panel of its own beside the menu it came from — in the same parent, so in the same
 * stacking context: on the body for a menu portaled there, inside a window for a menu drawn in one —
 * and placed by script. As a box inside its menu, overflowing it, it made the menu one layer the
 * size of both, with an empty corner over the glass beside them; over glass that left blocks of
 * stale colour on screen, as a menu's shadow did, while the menu itself, alone and solid, left none.
 * Each level is now the same single solid panel.
 *
 * Rendered where the submenu used to be, in its row: an empty, hidden marker stays there and is how
 * the panel finds its row and its menu. React events still bubble from the panel to the row, so the
 * menu's own handlers see the submenu's presses and keys as they did; a menu that listens on the
 * page instead asks `MenuSurfaces`.
 */
export function MenuFlyout({ className, hang = "down", autoFocus = false, children, ...rest }: MenuFlyoutProps) {
  const markerRef = useRef<HTMLSpanElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const surfaces = useContext(MenuSurfacesContext);
  /** Where the menu it came from is: the panel goes beside it. Known once the marker is in place. */
  const [container, setContainer] = useState<HTMLElement | null>(null);

  useLayoutEffect(() => {
    const row = markerRef.current?.parentElement;
    setContainer(row?.closest<HTMLElement>(PARENT_PANEL)?.parentElement ?? document.body);
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the panel exists once `container` does.
  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!panel || !surfaces) return;
    return surfaces.add(panel);
  }, [container, surfaces]);

  const placeRef = useRef<() => void>(() => undefined);
  placeRef.current = () => {
    const panel = panelRef.current;
    const row = markerRef.current?.parentElement;
    if (!panel || !row) return;
    const parent = row.closest<HTMLElement>(PARENT_PANEL) ?? row;
    const parentStyle = getComputedStyle(parent);
    const ownStyle = getComputedStyle(panel);
    const box = panel.getBoundingClientRect();
    const { left, top } = placeFlyout({
      row: row.getBoundingClientRect(),
      parent: parent.getBoundingClientRect(),
      parentBorder: { left: pixels(parentStyle.borderLeftWidth), right: pixels(parentStyle.borderRightWidth) },
      width: box.width,
      height: box.height,
      inset: {
        top: pixels(ownStyle.borderTopWidth) + pixels(ownStyle.paddingTop),
        bottom: pixels(ownStyle.borderBottomWidth) + pixels(ownStyle.paddingBottom)
      },
      hang,
      viewport: { width: window.innerWidth || box.right, height: window.innerHeight || box.bottom }
    });
    // Straight onto the element, before the frame is painted: a placement held in state would
    // render the panel once where it does not belong, or hidden, which also takes no focus.
    panel.style.left = `${left}px`;
    panel.style.top = `${top}px`;
    // Over a window, the menu is lifted over the window's scrim; its submenu goes with it.
    if (parentStyle.zIndex !== "auto") panel.style.zIndex = parentStyle.zIndex;
  };

  useLayoutEffect(() => placeRef.current());

  // What it lists can arrive or change size after it opened — a list read on opening, a picture.
  // biome-ignore lint/correctness/useExhaustiveDependencies: the panel exists once `container` does.
  useEffect(() => {
    const panel = panelRef.current;
    if (!panel || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => placeRef.current());
    observer.observe(panel);
    return () => observer.disconnect();
  }, [container]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the panel exists once `container` does.
  useEffect(() => {
    if (!autoFocus) return;
    panelRef.current?.querySelector<HTMLElement>("button:not(:disabled), input:not(:disabled)")?.focus();
  }, [autoFocus, container]);

  useFloatingSurface(panelRef, container !== null);

  return (
    <>
      <span ref={markerRef} hidden />
      {container && createPortal(
        <div ref={panelRef} className={className} {...rest}>
          {children}
        </div>,
        container
      )}
    </>
  );
}
