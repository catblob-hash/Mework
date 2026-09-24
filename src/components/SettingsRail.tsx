import { Check, Filter, Search, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { PropsWithChildren, ReactNode } from "react";
import { useI18n } from "../i18n";
import { ConfirmDeleteButton, IconButton } from "./Common";
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
  search,
  filter,
  sortableListId,
  footer,
  children
}: PropsWithChildren<{
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
    <aside className="provider-rail">
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
 * Rail delete action. The two-step confirm is the shared one; the rail only adds
 * the slot it shares with the status dot.
 */
export function SettingsRailDelete({
  name,
  disabled = false,
  onDelete
}: {
  /** Row name, used to build the accessible label. */
  name: string;
  disabled?: boolean;
  onDelete: () => void;
}) {
  const { t } = useI18n();

  return (
    <ConfirmDeleteButton
      label={t("删除 {name}", "Delete {name}", { name })}
      confirmLabel={t("确认删除 {name}", "Confirm deleting {name}", { name })}
      className="provider-rail__delete"
      disabled={disabled}
      onDelete={onDelete}
    />
  );
}

/** A rail row. */
export function SettingsRailRow({
  label,
  selected,
  active = false,
  onSelect
}: {
  label: string;
  selected: boolean;
  /** Green-dot condition: enabled provider or default new-conversation preset. */
  active?: boolean;
  onSelect: () => void;
}) {
  const trailing = (
    <span className="provider-rail__trailing">
      {active && <span className="provider-rail__dot" aria-hidden="true" />}
    </span>
  );

  return (
    <div className="provider-rail__slot">
      <button
        type="button"
        className="provider-rail__row"
        data-selected={selected ? "true" : "false"}
        aria-current={selected || undefined}
        onClick={onSelect}
      >
        <ProviderAvatar name={label} />
        <span className="provider-rail__name">{label}</span>
        {trailing}
      </button>
    </div>
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
