import { ChevronDown, MoreVertical, X } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type {
  CSSProperties,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  ReactNode
} from "react";
import { createPortal, flushSync } from "react-dom";
import { useI18n } from "../i18n";
import { IconButton } from "./Common";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuSection } from "./PopoverMenu";
import { reorderItems, usePointerDrag } from "./usePointerDrag";
import type { DragPoint, ReorderDropTarget } from "./usePointerDrag";
import { usePopoverAnchor } from "./usePopoverAnchor";
import "./PageTabs.css";

/** One page of a pane, as its tab. */
export interface PageTab {
  id: string;
  /** Plain text: the tab's label, its overflow row, its hover text and the name it is announced by. */
  label: string;
  /**
   * Drawn in the tab instead of `label`: the label plus something only a screen reader hears, say.
   * The strip is fitted to `label`, so this has to draw at the label's width.
   */
  content?: ReactNode;
  /** Hover text; defaults to `label`. */
  title?: string;
  /**
   * Drawn before the label. It is drawn once more, unseen, to measure the tab, so it has to be
   * presentational — an icon, not a control.
   */
  icon?: ReactNode;
  /** A short tag before the label, such as the number of the workspace a page belongs to. */
  badge?: string;
  /** Trailing secondary text on the tab's overflow-menu row, such as the directory a file is in. */
  hint?: string;
  /** Whether the tab has a close control. Defaults to true whenever the strip has `onClose`. */
  closable?: boolean;
  /** The close control is shown but spent: the page is already on its way out. */
  closeDisabled?: boolean;
  /** Extra class on the tab's box, for a caller's own variant of the tab. */
  className?: string;
}

export interface PageTabsProps {
  tabs: PageTab[];
  activeId: string | null;
  /** The tablist's accessible name. */
  ariaLabel: string;
  /** Names the overflow trigger and the menu it opens. */
  moreLabel: string;
  onSelect: (id: string) => void;
  onClose?: (id: string) => void;
  /** A tab's close control's accessible name; defaults to "Close <label>". */
  closeLabel?: (tab: PageTab) => string;
  /**
   * Delete and Backspace close the focused tab. Only for pages that cost nothing to reopen: where
   * closing ends something, a stray key on a focused tab must not.
   */
  closeOnDeleteKey?: boolean;
  /**
   * Makes the order the user's: a tab drags within the strip, within the overflow menu and between
   * the two, and Ctrl+Shift+Arrow moves the focused one a slot. Receives every id, in the new order.
   */
  onReorder?: (ids: string[]) => void;
  /** The DOM id of the panel a tab controls, for `aria-controls`. */
  panelId?: (id: string) => string;
  /** The widest a tab may be, in px; a longer label is ellipsized. */
  maxTabWidth?: number;
  /** A ⋮ menu on each tab, for acting on what the tab shows. */
  tabMenu?: (id: string) => PopoverMenuSection[];
  /** A tab's ⋮ menu's accessible name; defaults to "Actions for <label>". */
  tabMenuLabel?: (tab: PageTab) => string;
  /** An editor drawn in a tab in place of its label — a rename field — or null for the label. */
  renderEditor?: (tab: PageTab) => ReactNode | null;
  onTabDoubleClick?: (id: string) => void;
  /** Controls after the strip and its overflow trigger, such as the one that opens another page. */
  trailing?: ReactNode;
  className?: string;
}

/** What the strip measured: the room it has and the width each tab takes. */
export interface PageTabsMetrics {
  /** Each tab's width at rest, by id: its natural width, capped at the strip's maximum. */
  widths: ReadonlyMap<string, number>;
  /** Room for the tabs and the overflow trigger: the bar, less its trailing controls. */
  available: number;
  /** Space between neighbouring tabs, and between the strip and the trigger. */
  gap: number;
  /** The overflow trigger's width. */
  trigger: number;
}

export interface PageTabsFit {
  /** The tabs the strip draws, in the order drawn. */
  shown: string[];
  /** The rest, in the caller's order: the overflow menu's rows. */
  hidden: string[];
}

/** Sub-pixel slack, so a tab measured at 99.6px is not pushed out of a 99.5px gap by rounding. */
const FIT_TOLERANCE = 0.5;
/** How far outside a list a dragged tab still counts as over it. */
const DRAG_SLOP = 24;
const DRAG_EXCLUDED = "[data-drag-exclude]";

/**
 * Which tabs the strip draws and which go to the overflow menu.
 *
 * The strip shows the longest run from the start that fits, leaving room for the overflow trigger
 * whenever something does not. The active tab is never the one left out: when it falls past the
 * run it takes the run's last slot — more than one if it is the wider — for as long as it stays
 * active, without moving in the caller's order. With nothing measured (a pane not laid out yet, a
 * test environment with no layout) every tab is shown.
 */
export function fitPageTabs(
  ids: readonly string[],
  activeId: string | null,
  metrics: PageTabsMetrics | null
): PageTabsFit {
  if (!metrics || metrics.available <= 0 || ids.length === 0) return { shown: [...ids], hidden: [] };
  const { widths, available, gap, trigger } = metrics;
  const span = (run: readonly string[]) => run.reduce(
    (total, id, index) => total + (widths.get(id) ?? 0) + (index > 0 ? gap : 0),
    0
  );
  if (span(ids) <= available + FIT_TOLERANCE) return { shown: [...ids], hidden: [] };

  const budget = available - gap - trigger + FIT_TOLERANCE;
  let count = 0;
  let used = -gap;
  for (const id of ids) {
    const next = used + gap + (widths.get(id) ?? 0);
    if (next > budget) break;
    used = next;
    count += 1;
  }
  let shown = ids.slice(0, count);
  const activeIndex = activeId === null ? -1 : ids.indexOf(activeId);
  if (activeIndex >= count) {
    const active = ids[activeIndex];
    shown = shown.slice(0, Math.max(0, count - 1));
    while (shown.length > 0 && span([...shown, active]) > budget) shown.pop();
    shown.push(active);
  }
  // Too narrow for even one tab and the trigger: the one that matters most still shows, clipped.
  if (shown.length === 0) shown = [ids[0]];
  const drawn = new Set(shown);
  return { shown, hidden: ids.filter((id) => !drawn.has(id)) };
}

function sameMetrics(left: PageTabsMetrics, right: PageTabsMetrics): boolean {
  if (
    left.available !== right.available
    || left.gap !== right.gap
    || left.trigger !== right.trigger
    || left.widths.size !== right.widths.size
  ) return false;
  for (const [id, width] of left.widths) {
    if (right.widths.get(id) !== width) return false;
  }
  return true;
}

/**
 * The slot in `list` nearest the pointer, along the axis the list runs: before or after one of its
 * `[data-page-tab-slot]` children, the dragged one aside. Null once the pointer is more than `slop`
 * outside the list.
 */
function slotAt(
  list: HTMLElement | null,
  sourceId: string,
  point: DragPoint,
  axis: "x" | "y",
  slop: number
): ReorderDropTarget | null {
  if (!list) return null;
  const box = list.getBoundingClientRect();
  if (box.width <= 0 || box.height <= 0) return null;
  if (
    point.x < box.left - slop || point.x > box.right + slop
    || point.y < box.top - slop || point.y > box.bottom + slop
  ) return null;
  const coordinate = axis === "x" ? point.x : point.y;
  let nearest: { id: string; center: number; distance: number } | null = null;
  for (const element of list.querySelectorAll<HTMLElement>("[data-page-tab-slot]")) {
    const id = element.dataset.pageTabSlot;
    if (id === undefined || id === sourceId) continue;
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) continue;
    const center = axis === "x" ? rect.left + rect.width / 2 : rect.top + rect.height / 2;
    const distance = Math.abs(coordinate - center);
    if (!nearest || distance < nearest.distance) nearest = { id, center, distance };
  }
  if (!nearest) return null;
  return { id: nearest.id, position: coordinate < nearest.center ? "before" : "after" };
}

type DragSource = { id: string; from: "strip" | "menu" };
type DropSlot = ReorderDropTarget & { list: "strip" | "menu" };

function menuControls(panel: HTMLElement | null): HTMLElement[] {
  return Array.from(panel?.querySelectorAll<HTMLElement>("button:not(:disabled)") ?? []);
}

/**
 * The title bar of a pane that shows one page at a time — a shell, a preview page, an open file —
 * as a single row of tabs.
 *
 * The row never scrolls. Every tab is measured at rest in a hidden copy of the strip, and the
 * strip draws the longest run from the start that fits; the rest are rows of an overflow menu at
 * the strip's end, and the active tab is always among the drawn ones. With `onReorder`, the strip
 * and the menu are two views of one order: a tab drags within either and across between them.
 */
export function PageTabs({
  tabs,
  activeId,
  ariaLabel,
  moreLabel,
  onSelect,
  onClose,
  closeLabel,
  closeOnDeleteKey = false,
  onReorder,
  panelId,
  maxTabWidth = 180,
  tabMenu,
  tabMenuLabel,
  renderEditor,
  onTabDoubleClick,
  trailing,
  className
}: PageTabsProps) {
  const { t } = useI18n();
  const rootRef = useRef<HTMLDivElement>(null);
  const stripRef = useRef<HTMLDivElement>(null);
  const measureRef = useRef<HTMLDivElement>(null);
  const trailingRef = useRef<HTMLDivElement>(null);
  const [metrics, setMetrics] = useState<PageTabsMetrics | null>(null);
  /** A tab to focus once it is drawn: one selected from the keyboard or the menu. */
  const focusRequestRef = useRef<string | null>(null);
  /** The menu row to focus once the menu is placed, when it was opened from the keyboard. */
  const menuFocusRef = useRef<"first" | "last" | null>(null);
  const editingRef = useRef<string[]>([]);
  const restoreFrameRef = useRef(0);

  const ids = tabs.map((tab) => tab.id);
  const tabById = new Map(tabs.map((tab) => [tab.id, tab]));
  const currentId = activeId !== null && tabById.has(activeId) ? activeId : null;
  const { shown, hidden } = fitPageTabs(ids, currentId, metrics);
  const tabStopId = currentId ?? shown[0] ?? null;
  const hasTrailing = trailing !== undefined && trailing !== null && trailing !== false;
  const closable = (tab: PageTab) => onClose !== undefined && (tab.closable ?? true);
  const closeName = (tab: PageTab) => closeLabel?.(tab) ?? t("关闭 {name}", "Close {name}", { name: tab.label });

  const measure = useCallback(() => {
    const root = rootRef.current;
    const row = measureRef.current;
    if (!root || !row) return;
    const gap = Number.parseFloat(getComputedStyle(root).columnGap) || 0;
    const widths = new Map<string, number>();
    for (const element of row.querySelectorAll<HTMLElement>("[data-measure-tab]")) {
      widths.set(element.dataset.measureTab ?? "", element.getBoundingClientRect().width);
    }
    const trigger = row.querySelector<HTMLElement>("[data-measure-trigger]")?.getBoundingClientRect().width ?? 0;
    const trailingBox = trailingRef.current;
    const reserved = trailingBox && !trailingBox.hidden ? trailingBox.getBoundingClientRect().width + gap : 0;
    const next = { widths, available: root.clientWidth - reserved, gap, trigger };
    setMetrics((current) => (current && sameMetrics(current, next) ? current : next));
  }, []);

  // Keyed on what the hidden copy draws rather than on `tabs`: callers pass a fresh array every
  // render, and forcing a layout read on each would cost a reflow per keystroke elsewhere in the app.
  const measureKey = [
    maxTabWidth,
    tabMenu ? "menu" : "",
    ...tabs.map((tab) => [
      tab.id, tab.label, tab.badge ?? "", tab.icon ? "icon" : "", closable(tab) ? "close" : "", tab.className ?? ""
    ].join("\u0000"))
  ].join("\u0001");
  // biome-ignore lint/correctness/useExhaustiveDependencies: `measureKey` is what changes the widths `measure` reads.
  useLayoutEffect(() => {
    measure();
  }, [measure, measureKey]);

  // The bar's width, a font arriving late and the trailing controls all change the fit without
  // changing anything the key sees. The observer reports between layout and paint, and an update
  // it schedules would render after the paint: for a frame — every frame while a pane is being
  // resized — the old set of tabs would be drawn squeezed into the new width. Flushed, the new fit
  // is what gets painted.
  useEffect(() => {
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => flushSync(measure));
    for (const element of [rootRef.current, measureRef.current, trailingRef.current]) {
      if (element) observer.observe(element);
    }
    return () => observer.disconnect();
  }, [measure]);

  // Presses on a tab keep the menu open while tabs can be dragged into it; a plain click on one
  // closes it from the tab's own handler.
  const reorderable = onReorder !== undefined;
  const menu = usePopoverAnchor<HTMLButtonElement, HTMLDivElement>({
    align: "end",
    keepOpenOnPress: (target) => {
      if (!reorderable || !stripRef.current?.contains(target)) return false;
      const element = target instanceof Element ? target : target.parentElement;
      return !element?.closest(DRAG_EXCLUDED);
    }
  });
  const { close: closeMenu, panelRef: menuPanelRef, position: menuPosition } = menu;
  const menuOpen = menu.open && hidden.length > 0;
  useEffect(() => {
    if (menu.open && hidden.length === 0) closeMenu(false);
  }, [closeMenu, hidden.length, menu.open]);

  const dropSlotAt = (point: DragPoint, sourceId: string): DropSlot | null => {
    const panel = menuOpen ? menuPanelRef.current : null;
    const inMenu = (slop: number): DropSlot | null => {
      const slot = slotAt(panel, sourceId, point, "y", slop);
      return slot && { ...slot, list: "menu" };
    };
    // The menu hangs just below the strip, inside the strip's slop: a pointer over the menu is the
    // menu's, even where the only row in it is the one being dragged.
    const box = panel?.getBoundingClientRect();
    if (box && point.x >= box.left && point.x <= box.right && point.y >= box.top && point.y <= box.bottom) {
      return inMenu(0);
    }
    const inStrip = slotAt(stripRef.current, sourceId, point, "x", DRAG_SLOP);
    return inStrip ? { ...inStrip, list: "strip" } : inMenu(DRAG_SLOP);
  };

  const {
    activeItem: dragSource,
    dropTarget: dropSlot,
    bind: bindDrag,
    cancel: cancelDrag
  } = usePointerDrag<DragSource, DropSlot>({
    getTarget: (point, source) => dropSlotAt(point, source.id),
    onDrop: (source, slot) => {
      const next = reorderItems(ids, source.id, slot.id, slot.position, (id) => id);
      if (next.some((id, index) => id !== ids[index])) onReorder?.(next);
    }
  });

  // Escape ends a drag before it can reach the menu, whose own Escape handler would close the menu
  // and swallow the key, leaving the drag live with nothing left to drop into.
  const dragging = dragSource !== null;
  useEffect(() => {
    if (!dragging) return;
    const cancelWithEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      cancelDrag();
    };
    window.addEventListener("keydown", cancelWithEscape, true);
    return () => window.removeEventListener("keydown", cancelWithEscape, true);
  }, [cancelDrag, dragging]);

  const tabButton = (id: string) => (
    Array.from(stripRef.current?.querySelectorAll<HTMLElement>('[role="tab"]') ?? [])
      .find((element) => element.dataset.pageTab === id) ?? null
  );

  /** Focus follows a selection made from the keyboard or the menu, once the tab is on screen. */
  const focusTab = (id: string) => {
    const button = tabButton(id);
    if (button) button.focus({ preventScroll: true });
    else focusRequestRef.current = id;
  };

  const editingIds: string[] = [];
  const editors = new Map<string, ReactNode>();
  for (const id of shown) {
    const tab = tabById.get(id);
    const editor = tab && renderEditor ? renderEditor(tab) : null;
    if (editor === null || editor === undefined) continue;
    editingIds.push(id);
    editors.set(id, editor);
  }

  useLayoutEffect(() => {
    const request = focusRequestRef.current;
    focusRequestRef.current = null;
    if (request !== null) {
      // Only while the focus is still the strip's or the menu's, or was dropped with a menu row.
      const focused = document.activeElement;
      const ours = !focused || focused === document.body
        || rootRef.current?.contains(focused) || menuPanelRef.current?.contains(focused);
      if (ours) tabButton(request)?.focus({ preventScroll: true });
    }
    // An editor that closed on Enter or Escape took the focus with it. Hand it back to its tab —
    // a frame later, so an editor that closed because another control was clicked leaves the
    // focus with that control.
    const ended = editingRef.current.find((id) => !editingIds.includes(id));
    editingRef.current = editingIds;
    if (ended === undefined) return;
    window.cancelAnimationFrame(restoreFrameRef.current);
    restoreFrameRef.current = window.requestAnimationFrame(() => {
      if (document.activeElement === document.body) tabButton(ended)?.focus({ preventScroll: true });
    });
  });
  useEffect(() => () => window.cancelAnimationFrame(restoreFrameRef.current), []);

  useEffect(() => {
    const request = menuFocusRef.current;
    if (request === null || !menuOpen || !menuPosition) return;
    menuFocusRef.current = null;
    const controls = menuControls(menuPanelRef.current);
    (request === "first" ? controls[0] : controls[controls.length - 1])?.focus();
  }, [menuOpen, menuPanelRef, menuPosition]);

  const select = (id: string) => {
    if (menu.open) closeMenu(false);
    onSelect(id);
  };

  const closeTab = (tab: PageTab) => {
    if (closable(tab) && !tab.closeDisabled) onClose?.(tab.id);
  };

  const onMiddleClick = (event: ReactMouseEvent, tab: PageTab) => {
    if (event.button !== 1) return;
    event.preventDefault();
    closeTab(tab);
  };

  const onTabKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>, tab: PageTab) => {
    const index = ids.indexOf(tab.id);
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      const delta = event.key === "ArrowLeft" ? -1 : 1;
      if (event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey) {
        if (!onReorder) return;
        event.preventDefault();
        const destination = index + delta;
        if (destination < 0 || destination >= ids.length) return;
        const next = ids.filter((id) => id !== tab.id);
        next.splice(destination, 0, tab.id);
        onReorder(next);
        return;
      }
      if (event.ctrlKey || event.shiftKey || event.altKey || event.metaKey) return;
      event.preventDefault();
      const next = ids[(index + delta + ids.length) % ids.length];
      if (next === undefined) return;
      select(next);
      focusTab(next);
      return;
    }
    if (event.key === "Home" || event.key === "End") {
      const next = event.key === "Home" ? ids[0] : ids[ids.length - 1];
      if (next === undefined) return;
      event.preventDefault();
      select(next);
      focusTab(next);
      return;
    }
    if (closeOnDeleteKey && (event.key === "Delete" || event.key === "Backspace") && !event.altKey) {
      event.preventDefault();
      closeTab(tab);
    }
  };

  const onTriggerKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const edge = event.key === "ArrowDown" ? "first" : "last";
    if (!menu.open) {
      menuFocusRef.current = edge;
      menu.toggle();
      return;
    }
    const controls = menuControls(menuPanelRef.current);
    (edge === "first" ? controls[0] : controls[controls.length - 1])?.focus();
  };

  const onMenuKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const controls = menuControls(event.currentTarget);
    if (!controls.length) return;
    const current = controls.indexOf(document.activeElement as HTMLElement);
    const direction = event.key === "ArrowDown" ? 1 : -1;
    const next = current < 0
      ? (direction === 1 ? 0 : controls.length - 1)
      : (current + direction + controls.length) % controls.length;
    controls[next]?.focus();
  };

  const dragHandlers = (source: DragSource) => (reorderable ? bindDrag(source) : {});
  const dropClass = (list: DropSlot["list"], id: string, base: string) => (
    dropSlot && dropSlot.list === list && dropSlot.id === id ? ` ${base}--drop-${dropSlot.position}` : ""
  );

  const renderTab = (tab: PageTab) => {
    const active = tab.id === currentId;
    const editor = editors.get(tab.id);
    const boxClass = `page-tab${active ? " page-tab--active" : ""}${editor !== undefined ? " page-tab--editing" : ""}${
      dragSource?.id === tab.id ? " page-tab--dragging" : ""
    }${dropClass("strip", tab.id, "page-tab")}${tab.className ? ` ${tab.className}` : ""}`;
    const menuName = tabMenuLabel?.(tab) ?? t("{name} 的操作", "Actions for {name}", { name: tab.label });
    return (
      // The tab's controls cannot live inside the tab button, so they share a presentational box
      // the tablist looks straight through.
      <div key={tab.id} role="presentation" className={boxClass} data-page-tab-slot={tab.id}>
        {editor !== undefined ? editor : (
          <>
            <button
              type="button"
              role="tab"
              className="page-tab__label"
              data-page-tab={tab.id}
              aria-selected={active}
              aria-controls={panelId?.(tab.id)}
              tabIndex={tab.id === tabStopId ? 0 : -1}
              title={tab.title ?? tab.label}
              onClick={() => select(tab.id)}
              onDoubleClick={onTabDoubleClick ? () => onTabDoubleClick(tab.id) : undefined}
              // A middle press would otherwise start the platform's autoscroll.
              onMouseDown={(event) => {
                if (event.button === 1) event.preventDefault();
              }}
              onAuxClick={(event) => onMiddleClick(event, tab)}
              onKeyDown={(event) => onTabKeyDown(event, tab)}
              {...dragHandlers({ id: tab.id, from: "strip" })}
            >
              {tab.icon && <span className="page-tab__icon" aria-hidden="true">{tab.icon}</span>}
              {tab.badge && <span className="page-tab__badge" aria-hidden="true">{tab.badge}</span>}
              <span className="page-tab__text">{tab.content ?? tab.label}</span>
            </button>
            {tabMenu && (
              <PopoverMenu
                rootClassName="page-tab__menu"
                triggerClassName="icon-button page-tab__menu-trigger"
                trigger={<MoreVertical size={12} aria-hidden="true" />}
                triggerLabel={menuName}
                menuLabel={menuName}
                sections={tabMenu(tab.id)}
                align="end"
                dense
              />
            )}
            {closable(tab) && (
              <IconButton
                className="page-tab__close"
                label={closeName(tab)}
                disabled={tab.closeDisabled}
                // Keeps whatever the tab shows from taking a focus change on its way out.
                onMouseDown={(event) => event.preventDefault()}
                onClick={(event) => {
                  event.stopPropagation();
                  closeTab(tab);
                }}
              >
                <X size={11} aria-hidden="true" />
              </IconButton>
            )}
          </>
        )}
      </div>
    );
  };

  const renderMenuRow = (tab: PageTab) => (
    <div
      key={tab.id}
      className={`page-tabs__menu-row${dragSource?.id === tab.id ? " page-tabs__menu-row--dragging" : ""}${
        dropClass("menu", tab.id, "page-tabs__menu-row")
      }`}
      data-page-tab-slot={tab.id}
    >
      <button
        type="button"
        role="menuitem"
        className="popover-menu__item page-tabs__menu-item"
        title={tab.title ?? tab.label}
        onClick={() => {
          focusRequestRef.current = tab.id;
          select(tab.id);
        }}
        onMouseDown={(event) => {
          if (event.button === 1) event.preventDefault();
        }}
        onAuxClick={(event) => onMiddleClick(event, tab)}
        {...dragHandlers({ id: tab.id, from: "menu" })}
      >
        {tab.icon && <span className="page-tab__icon" aria-hidden="true">{tab.icon}</span>}
        {tab.badge && <span className="page-tab__badge" aria-hidden="true">{tab.badge}</span>}
        <span className="page-tabs__menu-label">{tab.label}</span>
        {tab.hint && <span className="popover-menu__hint page-tabs__menu-hint">{tab.hint}</span>}
      </button>
      {closable(tab) && (
        <button
          type="button"
          className="popover-menu__action page-tabs__menu-close"
          aria-label={closeName(tab)}
          title={closeName(tab)}
          data-drag-exclude
          disabled={tab.closeDisabled}
          onClick={() => closeTab(tab)}
        >
          <X size={12} aria-hidden="true" />
        </button>
      )}
    </div>
  );

  const rootStyle = { "--page-tab-max-width": `${maxTabWidth}px` } as CSSProperties;

  return (
    <div
      ref={rootRef}
      className={`page-tabs${reorderable ? " page-tabs--sortable" : ""}${className ? ` ${className}` : ""}`}
      style={rootStyle}
    >
      <div ref={stripRef} className="page-tabs__strip" role="tablist" aria-label={ariaLabel}>
        {shown.map((id) => {
          const tab = tabById.get(id);
          return tab ? renderTab(tab) : null;
        })}
      </div>
      {hidden.length > 0 && (
        <button
          ref={menu.triggerRef}
          type="button"
          className={`icon-button page-tabs__overflow-trigger${menuOpen ? " popover-menu__trigger--open" : ""}`}
          aria-label={moreLabel}
          title={moreLabel}
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          data-drag-exclude
          onClick={(event) => {
            // A keyboard press reports no clicks; hand it the first row, as the arrows would.
            if (!menu.open && event.detail === 0) menuFocusRef.current = "first";
            menu.toggle(event);
          }}
          onKeyDown={onTriggerKeyDown}
        >
          <ChevronDown size={13} aria-hidden="true" />
        </button>
      )}
      {menuOpen && createPortal(
        <div
          ref={menuPanelRef}
          role="menu"
          aria-label={moreLabel}
          className={`popover-menu__panel popover-menu__panel--dense page-tabs__menu${
            menuPosition?.flipped ? " popover-menu__panel--flipped" : ""
          }`}
          style={{
            left: menuPosition?.left ?? 0,
            top: menuPosition?.top ?? 0,
            zIndex: menuPosition?.layer,
            visibility: menuPosition ? "visible" : "hidden"
          }}
          onKeyDown={onMenuKeyDown}
        >
          <div className="popover-menu__list">
            {hidden.map((id) => {
              const tab = tabById.get(id);
              return tab ? renderMenuRow(tab) : null;
            })}
          </div>
        </div>,
        document.body
      )}
      <div ref={trailingRef} className="page-tabs__trailing" hidden={!hasTrailing}>
        {trailing}
      </div>
      {/* Every tab at rest, unseen and pushing nothing, for the fit to read widths from. Labels are
          drawn from attributes so the copy adds no text a screen reader or a text query could find. */}
      <div className="page-tabs__measure" aria-hidden="true">
        <div ref={measureRef} className="page-tabs__measure-row">
          {tabs.map((tab) => (
            <div
              key={tab.id}
              className={`page-tab${tab.className ? ` ${tab.className}` : ""}`}
              data-measure-tab={tab.id}
            >
              <span className="page-tab__label">
                {tab.icon && <span className="page-tab__icon">{tab.icon}</span>}
                {tab.badge && <span className="page-tab__badge" data-measure-text={tab.badge} />}
                <span className="page-tab__text" data-measure-text={tab.label} />
              </span>
              {tabMenu && <span className="page-tab__menu-trigger" />}
              {closable(tab) && <span className="page-tab__close" />}
            </div>
          ))}
          <span className="page-tabs__overflow-trigger" data-measure-trigger="" />
        </div>
      </div>
    </div>
  );
}
