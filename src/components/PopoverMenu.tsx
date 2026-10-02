import { Check, ChevronRight, Search } from "lucide-react";
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { MenuFlyout, MenuSurfacesContext, createMenuSurfaces } from "./MenuFlyout";
import { PathText } from "./PathText";
import { usePopoverAnchor } from "./usePopoverAnchor";

/** A copy of the open panel's measured box, by value. */
export type PopoverPanelRect = Pick<DOMRect, "x" | "y" | "left" | "top" | "right" | "bottom" | "width" | "height">;

/**
 * A small icon button trailing a row or a section heading — a gear that opens
 * the settings of what the row names, say. It is a sibling of the row's own
 * button, never inside it, and choosing it closes the menu like choosing the row.
 */
export interface PopoverMenuAction {
  /** Accessible name and tooltip; the button shows only its icon. */
  label: string;
  icon: ReactNode;
  onSelect: () => void;
}

/** A row in the menu. */
export interface PopoverMenuItem {
  id: string;
  label: string;
  /** Secondary text explaining this item or why it is disabled. */
  description?: string;
  /** The description is a path, drawn so that it gives way in the middle. */
  descriptionIsPath?: boolean;
  icon?: ReactNode;
  /** Subtle trailing text, such as a shortcut or why a setting is not taking effect. */
  hint?: string;
  /** A small trailing mark about the row's state, such as a warm cache; it carries its own accessible name. */
  badge?: ReactNode;
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
  /** A trailing button beside the row, for acting on what the row names rather than choosing it. */
  action?: PopoverMenuAction;
}

/** A group of rows with an optional heading. Adjacent groups are separated. */
export interface PopoverMenuSection {
  id: string;
  label?: string;
  /** A trailing button on the heading, for acting on what the whole group shares. Needs a `label`. */
  action?: PopoverMenuAction;
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
  /**
   * The side of the trigger the menu prefers; see `PopoverAnchorOptions.placement`. Defaults to
   * `below`. `above` suits a trigger with more content underneath it, such as a row in a list
   * that grows downward, and still falls back below when there is no room above.
   */
  placement?: "below" | "above";
  /** Extra class on the portaled panel, e.g. to lift a menu above a modal dialog's backdrop. */
  panelClassName?: string;
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
}

function matchesQuery(item: PopoverMenuItem, query: string): boolean {
  if (!query) return true;
  const needle = query.toLowerCase();
  if (item.label.toLowerCase().includes(needle)) return true;
  if (item.description?.toLowerCase().includes(needle)) return true;
  return Boolean(item.children?.some((child) => matchesQuery(child, query)));
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
  placement = "below",
  panelClassName = "",
  dense = false,
  anchorToPointer = false,
  searchPlaceholder,
  emptyLabel,
  submenu = "inline",
  onOpen,
  onPanelRectChange
}: PopoverMenuProps) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [expandedId, setExpandedId] = useState<string | null>(null);
  /** The open submenu was opened from the keyboard, so it takes the focus. */
  const [expandedByKey, setExpandedByKey] = useState(false);
  const branchButtons = useRef(new Map<string, HTMLButtonElement>());
  const [surfaces] = useState(createMenuSurfaces);
  const { open, position, triggerRef, panelRef, toggle, close } = usePopoverAnchor({
    align,
    placement,
    width: menuWidth,
    anchorToPointer,
    onOpen,
    // A submenu is a panel of its own on the page, not inside the menu's.
    keepOpenOnPress: surfaces.contains
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

  const expand = (id: string | null, byKey: boolean) => {
    setExpandedByKey(byKey);
    setExpandedId(id);
  };

  /** A submenu's own keys: the arrows move through it, and left goes back to its row. */
  const onFlyoutKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>, item: PopoverMenuItem) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      moveFocus(event.currentTarget, event.key === "ArrowDown" ? 1 : -1);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      event.stopPropagation();
      expand(null, false);
      branchButtons.current.get(item.id)?.focus();
    }
  };

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

  const renderAction = (action: PopoverMenuAction) => (
    <button
      type="button"
      className="popover-menu__action"
      aria-label={action.label}
      title={action.label}
      onClick={() => {
        close(false);
        action.onSelect();
      }}
    >
      {action.icon}
    </button>
  );

  const renderItem = (item: PopoverMenuItem, depth: number): ReactNode => {
    const expandable = Boolean(item.children?.length);
    const expanded = expandable && expandedId === item.id;
    const nested = expanded && (
      submenu === "flyout"
        ? (
          <MenuFlyout
            className={`popover-menu__panel popover-menu__panel--flyout popover-menu__flyout${dense ? " popover-menu__panel--dense" : ""}`}
            role="menu"
            aria-label={item.label}
            autoFocus={expandedByKey}
            onKeyDown={(event) => onFlyoutKeyDown(event, item)}
          >
            {item.children?.map((child) => renderItem(child, depth + 1))}
          </MenuFlyout>
        )
        : <div className="popover-menu__submenu" role="menu" aria-label={item.label}>
          {item.children?.map((child) => renderItem(child, depth + 1))}
        </div>
    );
    const button = (
      <button
        type="button"
        ref={expandable && submenu === "flyout"
          ? (element) => {
            if (element) branchButtons.current.set(item.id, element);
            else branchButtons.current.delete(item.id);
          }
          : undefined}
        role={item.checked === undefined
          ? "menuitem"
          : item.checkedRole === "checkbox" ? "menuitemcheckbox" : "menuitemradio"}
        aria-checked={item.checked === undefined ? undefined : item.checked}
        aria-haspopup={expandable ? "menu" : undefined}
        aria-expanded={expandable ? expanded : undefined}
        className={`popover-menu__item${depth > 0 ? " popover-menu__item--nested" : ""}`}
        disabled={item.disabled}
        title={item.title}
        onClick={(event) => {
          if (expandable) {
            // A keyboard press reports no clicks.
            expand(expandedId === item.id ? null : item.id, event.detail === 0);
            return;
          }
          item.onSelect?.();
          close(false);
        }}
        onKeyDown={expandable && submenu === "flyout"
          ? (event) => {
            if (event.key !== "ArrowRight") return;
            event.preventDefault();
            event.stopPropagation();
            expand(item.id, true);
          }
          : undefined}
      >
        {item.icon && <span className="popover-menu__icon">{item.icon}</span>}
        <span className="popover-menu__copy">
          <strong>{item.label}</strong>
          {item.description && (
            <small>{item.descriptionIsPath ? <PathText path={item.description} /> : item.description}</small>
          )}
        </span>
        {item.hint && <span className="popover-menu__hint">{item.hint}</span>}
        {item.checked && <Check size={14} className="popover-menu__check" />}
        {/* After the checkmark, so badges line up at the edge whichever row is checked. */}
        {item.badge}
        {expandable && (
          <ChevronRight
            size={13}
            className={`popover-menu__chevron${
              expanded && submenu === "inline" ? " popover-menu__chevron--open" : ""
            }`}
          />
        )}
      </button>
    );
    return (
      <div
        className={`popover-menu__row${
          submenu === "flyout" && expandable ? " popover-menu__row--branch" : ""
        }`}
        key={item.id}
      >
        {item.action
          ? <div className="popover-menu__line">{button}{renderAction(item.action)}</div>
          : button}
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
        <MenuSurfacesContext.Provider value={surfaces}>
          <div
            ref={panelRef}
            className={`popover-menu__panel${position?.flipped ? " popover-menu__panel--flipped" : ""}${dense ? " popover-menu__panel--dense" : ""}${submenu === "flyout" ? " popover-menu__panel--flyout" : ""}${panelClassName ? ` ${panelClassName}` : ""}`}
            role="menu"
            aria-label={menuLabel}
            style={{
              left: position?.left ?? 0,
              top: position?.top ?? 0,
              width: menuWidth,
              minWidth: position?.minWidth,
              zIndex: position?.layer,
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
                  {section.label && (section.action
                    ? (
                      <div className="popover-menu__label popover-menu__label--action">
                        <span>{section.label}</span>
                        {renderAction(section.action)}
                      </div>
                    )
                    : <div className="popover-menu__label">{section.label}</div>)}
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
          </div>
        </MenuSurfacesContext.Provider>,
        document.body
      )}
    </div>
  );
}
