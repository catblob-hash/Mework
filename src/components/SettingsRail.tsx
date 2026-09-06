import { Check, Filter, MoreVertical, Search, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { PropsWithChildren, ReactNode, RefObject } from "react";
import { useI18n } from "../i18n";
import { IconButton } from "./Common";
import { ProviderAvatar } from "./ProviderSettings/ProviderAvatar";
import "./ProviderSettings/ProviderSettings.css";

/**
 * Shared settings-page rail: search and filtering, scrolling list, and fixed footer.
 * Each page supplies row contents; legacy `provider-rail__*` class names retain the
 * existing styles in `ProviderSettings.css`.
 */

export interface SettingsRailSearch {
  value: string;
  onChange: (value: string) => void;
  /** Accessible input name; also used as the fallback placeholder. */
  label: string;
  placeholder?: string;
}

export interface SettingsRailFilter {
  /** Accessible label for the filter button. */
  label: string;
  value: string;
  /** Equal to this value when filtering is inactive. */
  neutralValue: string;
  options: Array<{ value: string; label: string }>;
  onChange: (value: string) => void;
}

export function SettingsRail({
  railRef,
  search,
  filter,
  sortableListId,
  footer,
  children
}: PropsWithChildren<{
  /** Caller-supplied rail ref for hit testing, such as closing a provider kebab menu. */
  railRef?: RefObject<HTMLElement | null>;
  /** Omit to hide search; fixed catalog rows do not need it. */
  search?: SettingsRailSearch;
  /** Rendered only when `search` is also provided. */
  filter?: SettingsRailFilter;
  /** Adds `data-sortable-list` to the scroller for drag-sort targeting. */
  sortableListId?: string;
  footer?: ReactNode;
}>) {
  const { t } = useI18n();
  const [filterOpen, setFilterOpen] = useState(false);
  const filterRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!filterOpen) return;
    const close = (event: MouseEvent) => {
      if (!filterRef.current?.contains(event.target as Node)) setFilterOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setFilterOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [filterOpen]);

  return (
    <aside className="provider-rail" ref={railRef}>
      {search && (
        <div className="provider-rail__search">
          <div className="provider-rail__search-box">
            <Search size={13} />
            <input
              aria-label={search.label}
              value={search.value}
              onChange={(event) => search.onChange(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape" && search.value) {
                  event.stopPropagation();
                  search.onChange("");
                }
              }}
              placeholder={search.placeholder ?? search.label}
            />
            {search.value && (
              <IconButton
                label={t("清空搜索", "Clear search")}
                className="provider-rail__search-clear"
                onClick={() => search.onChange("")}
              ><X size={12} /></IconButton>
            )}
            {filter && (
              <div className="provider-rail__filter" ref={filterRef}>
                <IconButton
                  label={filter.label}
                  className={filter.value === filter.neutralValue
                    ? "provider-rail__search-clear"
                    : "provider-rail__search-clear provider-rail__search-clear--active"}
                  aria-haspopup="menu"
                  aria-expanded={filterOpen}
                  onClick={() => setFilterOpen((current) => !current)}
                ><Filter size={12} /></IconButton>
                {filterOpen && (
                  <div className="provider-rail__filter-menu" role="menu">
                    {filter.options.map((option) => (
                      <button
                        key={option.value}
                        type="button"
                        role="menuitemradio"
                        aria-checked={filter.value === option.value}
                        className="provider-rail__menu-item"
                        onClick={() => {
                          filter.onChange(option.value);
                          setFilterOpen(false);
                        }}
                      >
                        <Check size={13} className={filter.value === option.value ? "" : "provider-rail__menu-check--hidden"} />
                        {option.label}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}

      <div className="provider-rail__scroller" data-sortable-list={sortableListId}>
        {children}
      </div>

      {footer && <div className="provider-rail__footer">{footer}</div>}
    </aside>
  );
}

/**
 * A rail row.
 *
 * Rows with menus use a `role="button"` div because a button cannot contain the
 * inline kebab button.
 */
export function SettingsRailRow({
  label,
  selected,
  active = false,
  onSelect,
  title,
  menu
}: {
  label: string;
  selected: boolean;
  /** Green-dot condition: enabled provider or default new-conversation preset. */
  active?: boolean;
  onSelect: () => void;
  title?: string;
  /** Items in the trailing kebab menu. `close` hides the menu after selection. */
  menu?: (close: () => void) => ReactNode;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const slotRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      const target = event.target as Node;
      if (!slotRef.current?.querySelector(".provider-rail__menu")?.contains(target)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [open]);

  const trailing = (
    <span className="provider-rail__trailing">
      {active && <span className="provider-rail__dot" aria-hidden="true" />}
      {menu && (
        <IconButton
          label={t("{name} 的更多操作", "More actions for {name}", { name: label })}
          className="provider-rail__kebab"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={(event) => {
            event.stopPropagation();
            setOpen((current) => !current);
          }}
        ><MoreVertical size={13} /></IconButton>
      )}
    </span>
  );

  if (!menu) {
    return (
      <div className="provider-rail__slot">
        <button
          type="button"
          className="provider-rail__row"
          data-selected={selected ? "true" : "false"}
          aria-current={selected || undefined}
          title={title}
          onClick={onSelect}
        >
          <ProviderAvatar name={label} />
          <span className="provider-rail__name">{label}</span>
          {trailing}
        </button>
      </div>
    );
  }

  return (
    <div className="provider-rail__slot" ref={slotRef}>
      {/* biome-ignore lint/a11y/useSemanticElements: the inline kebab prevents a button wrapper. */}
      <div
        role="button"
        tabIndex={0}
        data-selected={selected ? "true" : "false"}
        aria-current={selected || undefined}
        // The nested kebab would otherwise be included in the computed accessible name.
        aria-label={label}
        title={title}
        className="provider-rail__row"
        onClick={onSelect}
        onKeyDown={(event) => {
          if (event.currentTarget !== event.target) return;
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onSelect();
          }
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          setOpen(true);
        }}
      >
        <ProviderAvatar name={label} />
        <span className="provider-rail__name">{label}</span>
        {trailing}
      </div>
      {open && (
        <div className="provider-rail__menu" role="menu">
          {menu(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}

export function SettingsRailMenuItem({
  danger = false,
  disabled = false,
  onClick,
  children
}: PropsWithChildren<{ danger?: boolean; disabled?: boolean; onClick: () => void }>) {
  return (
    <button
      type="button"
      role="menuitem"
      className={danger ? "provider-rail__menu-item provider-rail__menu-item--danger" : "provider-rail__menu-item"}
      disabled={disabled}
      onClick={onClick}
    >{children}</button>
  );
}

/** Empty rail state for no entries or no search matches. */
export function SettingsRailEmpty({
  icon,
  title,
  description
}: {
  icon: ReactNode;
  title: string;
  description?: string;
}) {
  return (
    <div className="provider-rail__empty">
      {icon}
      <strong>{title}</strong>
      {description && <span>{description}</span>}
    </div>
  );
}
