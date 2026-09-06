import { Check, ChevronRight, Search } from "lucide-react";
import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { usePopoverAnchor } from "./usePopoverAnchor";

/** A row in the menu. */
export interface PopoverMenuItem {
  id: string;
  label: string;
  /** Secondary text explaining this item or why it is disabled. */
  description?: string;
  icon?: ReactNode;
  /** Subtle trailing hint, such as a shortcut; `checked` takes its place. */
  hint?: string;
  /**
   * A defined value renders a `menuitemradio`; `true` shows a checkmark. `undefined` is an action
   * rendered as a regular `menuitem`.
   */
  checked?: boolean;
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
  /** Renders a search field next to the trigger when provided. */
  searchPlaceholder?: string;
  /** Message shown when search removes every row. */
  emptyLabel?: string;
  /** Invoked once whenever the menu opens, for lazy list loading. */
  onOpen?: () => void;
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
  searchPlaceholder,
  emptyLabel,
  onOpen
}: PopoverMenuProps) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const { open, position, triggerRef, panelRef, toggle, close } = usePopoverAnchor({
    align,
    width: menuWidth,
    onOpen
  });

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
    return (
      <div className="popover-menu__row" key={item.id}>
        <button
          type="button"
          role={item.checked === undefined ? "menuitem" : "menuitemradio"}
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
          {item.checked
            ? <Check size={14} className="popover-menu__check" />
            : item.hint
              ? <span className="popover-menu__hint">{item.hint}</span>
              : null}
          {expandable && (
            <ChevronRight
              size={13}
              className={`popover-menu__chevron${expanded ? " popover-menu__chevron--open" : ""}`}
            />
          )}
        </button>
        {expanded && (
          <div className="popover-menu__submenu" role="menu" aria-label={item.label}>
            {item.children?.map((child) => renderItem(child, depth + 1))}
          </div>
        )}
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
          className={`popover-menu__panel${position?.flipped ? " popover-menu__panel--flipped" : ""}`}
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
