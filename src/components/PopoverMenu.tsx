import { Check, ChevronRight, Search } from "lucide-react";
import type { ReactNode } from "react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { usePopoverAnchor } from "./usePopoverAnchor";

/** A copy of the open panel's measured box, by value. */
export type PopoverPanelRect = Pick<DOMRect, "x" | "y" | "left" | "top" | "right" | "bottom" | "width" | "height">;

/** A row in the menu. */
export interface PopoverMenuItem {
  id: string;
  label: string;
  /** Secondary text explaining this item or why it is disabled. */
  description?: string;
  icon?: ReactNode;
  /** Subtle trailing text, such as a shortcut or why a setting is not taking effect. */
  hint?: string;
  /**
   * A defined value renders a checked row; `true` shows a checkmark. `undefined` is an action
   * rendered as a regular `menuitem`.
   */
  checked?: boolean;
  /**
   * What the checkmark means. A `radio` row is one of a set where exactly one is picked; a
   * `checkbox` row is a setting that stands alone. Defaults to `radio`, which is what a row
   * that names a choice among siblings almost always is.
   */
  checkedRole?: "radio" | "checkbox";
  disabled?: boolean;
  title?: string;
  /** Expands into a nested list. Clicking an item with children only expands it. */
  children?: PopoverMenuItem[];
  onSelect?: () => void;
}

/** A group of rows with an optional heading. Adjacent groups are separated. */
export interface PopoverMenuSection {
  id: string;
  label?: string;
  items: PopoverMenuItem[];
}

export interface PopoverMenuProps {
  /** Trigger content. This component renders the trigger button. */
  trigger: ReactNode;
  /** Accessible name and default `title` for the trigger; include the current selection. */
  triggerLabel: string;
  triggerTitle?: string;
  triggerClassName?: string;
  rootClassName?: string;
  disabled?: boolean;
  sections: PopoverMenuSection[];
  menuLabel: string;
  /** Menu width in px. Defaults to content width but never less than the trigger. */
  menuWidth?: number;
  /** Alignment edge between the menu and trigger. */
  align?: "start" | "end";
  /** Single-line rows with tighter metrics, for menus that carry no descriptions. */
  dense?: boolean;
  /** Open with the panel's top-left corner at the pointer instead of below the trigger. */
  anchorToPointer?: boolean;
  /** Renders a search field next to the trigger when provided. */
  searchPlaceholder?: string;
  /** Message shown when search removes every row. */
  emptyLabel?: string;
  /**
   * How a row with `children` opens them. `inline` nests them under the row,
   * inside the panel's own scroll. `flyout` opens them beside the row, which is
   * what a choice whose second step is a refinement of the first wants: the
   * chosen branch stays on screen next to what it refines.
   */
  submenu?: "inline" | "flyout";
  /** Invoked once whenever the menu opens, for lazy list loading. */
  onOpen?: () => void;
  /**
   * Reports the open panel's viewport rectangle, and null once it closes. A pane whose body is a
   * native page stacked beneath the renderer needs the rectangle to tell when the panel covers
   * the page. Must be referentially stable.
   */
  onPanelRectChange?: (rect: PopoverPanelRect | null) => void;
  /** Opens the menu whenever this value changes; the first value is the baseline. */
  openSignal?: number;
}

function matchesQuery(item: PopoverMenuItem, query: string): boolean {
  if (!query) return true;
  const needle = query.toLowerCase();
  if (item.label.toLowerCase().includes(needle)) return true;
  if (item.description?.toLowerCase().includes(needle)) return true;
  return Boolean(item.children?.some((child) => matchesQuery(child, query)));
}

/**
 * A nested list that opens beside the row that owns it.
 *
 * It measures itself once, where it was drawn, and moves only if it would leave
 * the window: to the other side of its row when the right edge is too close,
 * and upward by however much hangs below the bottom. Measuring again after
 * moving would let a flipped panel decide to flip back.
 */
function PopoverFlyout({ label, children }: { label: string; children: ReactNode }) {
  const panelRef = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState<{ flipped: boolean; shift: number }>({
    flipped: false,
    shift: 0
  });
  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    // A window that reports no size — an unrendered preview — cannot say the
    // panel has left it, and treating 0 as the edge throws it off screen.
    if (!window.innerWidth || !window.innerHeight) return;
    const box = panel.getBoundingClientRect();
    const flipped = box.right > window.innerWidth - 8;
    const shift = Math.min(0, window.innerHeight - 8 - box.bottom);
    if (flipped || shift) setPlacement({ flipped, shift });
  }, []);
  return (
    <div
      ref={panelRef}
      role="menu"
      aria-label={label}
      className={`popover-menu__submenu popover-menu__submenu--flyout${
        placement.flipped ? " popover-menu__submenu--flipped" : ""
      }`}
      style={placement.shift ? { marginTop: placement.shift } : undefined}
    >
      {children}
    </div>
  );
}

/**
 * Shared application popover menu.
 *
 * The panel is portaled to `document.body`; `usePopoverAnchor` owns its position and dismissal.
 * This component only manages content, search, nested expansion, and clean state on reopening.
 */
export function PopoverMenu({
  trigger,
  triggerLabel,
  triggerTitle,
  triggerClassName = "",
  rootClassName = "",
  disabled = false,
  sections,
  menuLabel,
  menuWidth,
  align = "start",
  dense = false,
  anchorToPointer = false,
  searchPlaceholder,
  emptyLabel,
  submenu = "inline",
  onOpen,
  onPanelRectChange,
  openSignal
}: PopoverMenuProps) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const { open, position, triggerRef, panelRef, toggle, close } = usePopoverAnchor({
    align,
    width: menuWidth,
    anchorToPointer,
    onOpen,
    openSignal
  });
  const publishedRect = useRef(false);

  // `position` is null for the first frame of an open, so the panel is only ever reported once
  // it is placed — the page must never be treated as covered by a rectangle it has not taken yet.
  useEffect(() => {
    if (!onPanelRectChange) return;
    const panel = open && position ? panelRef.current : null;
    if (!panel) {
      if (!publishedRect.current) return;
      publishedRect.current = false;
      onPanelRectChange(null);
      return;
    }
    const rect = panel.getBoundingClientRect();
    publishedRect.current = true;
    // A DOMRect keeps its fields on the prototype, so a spread would hand over an empty object.
    onPanelRectChange({
      x: rect.x, y: rect.y, left: rect.left, top: rect.top,
      right: rect.right, bottom: rect.bottom, width: rect.width, height: rect.height
    });
  }, [open, position, onPanelRectChange, panelRef]);

  // Reset search and nested expansion when the menu closes.
  useEffect(() => {
    if (open) return;
    setQuery("");
    setExpandedId(null);
  }, [open]);

  const moveFocus = (container: HTMLElement, direction: 1 | -1) => {
    const focusable = Array.from(
      container.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled)')
    );
    if (!focusable.length) return;
    const current = focusable.indexOf(document.activeElement as HTMLElement);
    const next = current < 0
      ? (direction === 1 ? 0 : focusable.length - 1)
      : (current + direction + focusable.length) % focusable.length;
    focusable[next]?.focus();
  };

  const renderItem = (item: PopoverMenuItem, depth: number): ReactNode => {
    const expandable = Boolean(item.children?.length);
    const expanded = expandable && expandedId === item.id;
    const nested = expanded && (
      submenu === "flyout"
        ? <PopoverFlyout label={item.label}>
          {item.children?.map((child) => renderItem(child, depth + 1))}
        </PopoverFlyout>
        : <div className="popover-menu__submenu" role="menu" aria-label={item.label}>
          {item.children?.map((child) => renderItem(child, depth + 1))}
        </div>
    );
    return (
      <div
        className={`popover-menu__row${
          submenu === "flyout" && expandable ? " popover-menu__row--branch" : ""
        }`}
        key={item.id}
      >
        <button
          type="button"
          role={item.checked === undefined
            ? "menuitem"
            : item.checkedRole === "checkbox" ? "menuitemcheckbox" : "menuitemradio"}
          aria-checked={item.checked === undefined ? undefined : item.checked}
          aria-haspopup={expandable ? "menu" : undefined}
          aria-expanded={expandable ? expanded : undefined}
          className={`popover-menu__item${depth > 0 ? " popover-menu__item--nested" : ""}`}
          disabled={item.disabled}
          title={item.title}
          onClick={() => {
            if (expandable) {
              setExpandedId((current) => (current === item.id ? null : item.id));
              return;
            }
            item.onSelect?.();
            close(false);
          }}
        >
          {item.icon && <span className="popover-menu__icon">{item.icon}</span>}
          <span className="popover-menu__copy">
            <strong>{item.label}</strong>
            {item.description && <small>{item.description}</small>}
          </span>
          {item.hint && <span className="popover-menu__hint">{item.hint}</span>}
          {item.checked && <Check size={14} className="popover-menu__check" />}
          {expandable && (
            <ChevronRight
              size={13}
              className={`popover-menu__chevron${
                expanded && submenu === "inline" ? " popover-menu__chevron--open" : ""
              }`}
            />
          )}
        </button>
        {nested}
      </div>
    );
  };

  const visibleSections = sections
    .map((section) => ({
      ...section,
      items: section.items.filter((item) => matchesQuery(item, query))
    }))
    .filter((section) => section.items.length > 0);
  const empty = visibleSections.length === 0;

  const searchField = searchPlaceholder !== undefined && (
    <div className="popover-menu__search">
      <Search size={13} />
      <input
        type="text"
        value={query}
        placeholder={searchPlaceholder}
        aria-label={searchPlaceholder}
        onChange={(event) => setQuery(event.target.value)}
      />
    </div>
  );

  return (
    <div className={`popover-menu ${rootClassName}`.trim()}>
      <button
        ref={triggerRef}
        type="button"
        data-drag-exclude
        className={`${triggerClassName} ${open ? "popover-menu__trigger--open" : ""}`.trim()}
        aria-label={triggerLabel}
        title={triggerTitle ?? triggerLabel}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={toggle}
      >
        {trigger}
      </button>
      {open && createPortal(
        <div
          ref={panelRef}
          className={`popover-menu__panel${position?.flipped ? " popover-menu__panel--flipped" : ""}${dense ? " popover-menu__panel--dense" : ""}${submenu === "flyout" ? " popover-menu__panel--flyout" : ""}`}
          role="menu"
          aria-label={menuLabel}
          style={{
            left: position?.left ?? 0,
            top: position?.top ?? 0,
            width: menuWidth,
            minWidth: position?.minWidth,
            visibility: position ? "visible" : "hidden"
          }}
          onKeyDown={(event) => {
            if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
            event.preventDefault();
            moveFocus(event.currentTarget, event.key === "ArrowDown" ? 1 : -1);
          }}
        >
          {position?.flipped === false && searchField}
          <div className="popover-menu__list">
            {visibleSections.map((section, index) => (
              <div className="popover-menu__section" key={section.id}>
                {index > 0 && <div className="popover-menu__divider" />}
                {section.label && <div className="popover-menu__label">{section.label}</div>}
                {section.items.map((item) => renderItem(item, 0))}
              </div>
            ))}
            {empty && (
              <p className="popover-menu__empty">
                {emptyLabel ?? t("没有匹配项", "No matches")}
              </p>
            )}
          </div>
          {position?.flipped !== false && searchField}
        </div>,
        document.body
      )}
    </div>
  );
}
