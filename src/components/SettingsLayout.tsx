import type { LucideIcon } from "lucide-react";
import { Fragment, type ReactNode } from "react";
import { DialogSidebarTitle } from "./Common";

/**
 * The layout every settings window shares: global settings, a preset's window
 * and a role's window. A rail of pages down the left edge under the window's
 * name, the selected page on the right. One component rather than three copies
 * of the same classes, so a change to one window's frame lands on all of them.
 *
 * The conversation-settings side pane is not one of these: it is a few hundred
 * pixels wide and keeps its own compact rail.
 */
export interface SettingsNavigationItem<View extends string> {
  id: View;
  icon: LucideIcon;
  label: string;
  /** Trailed after the label; null or omitted draws nothing. */
  count?: number | null;
}

export interface SettingsNavigationGroup<View extends string> {
  id: string;
  /** Non-interactive and non-collapsible; a rail with one group has none. */
  title?: string;
  items: ReadonlyArray<SettingsNavigationItem<View>>;
}

export function SettingsNavigation<View extends string>({
  label,
  groups,
  view,
  onSelect,
  footer
}: {
  label: string;
  groups: ReadonlyArray<SettingsNavigationGroup<View>>;
  view: View;
  onSelect: (view: View) => void;
  /**
   * What belongs to the whole window rather than to one page — a preset's or a
   * role's save — kept at the foot of the rail, where the conversation-settings
   * pane keeps its own.
   */
  footer?: ReactNode;
}) {
  return (
    <nav className="settings-nav" aria-label={label}>
      <DialogSidebarTitle />
      {groups.map((group) => (
        <Fragment key={group.id}>
          {group.title && <div className="settings-nav__group-title">{group.title}</div>}
          {group.items.map((item) => {
            const Icon = item.icon;
            const active = view === item.id;
            return (
              <button
                type="button"
                key={item.id}
                aria-current={active || undefined}
                className={active ? "settings-nav__item settings-nav__item--active" : "settings-nav__item"}
                onClick={() => onSelect(item.id)}
              >
                <Icon size={16} aria-hidden="true" />
                <span>{item.label}</span>
                {item.count == null ? null : <small className="settings-nav__count">{item.count}</small>}
              </button>
            );
          })}
        </Fragment>
      ))}
      {footer && <div className="settings-nav__footer">{footer}</div>}
    </nav>
  );
}

export function SettingsLayout({
  label,
  navigation,
  children
}: {
  label: string;
  navigation: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="settings-layout" aria-label={label}>
      {navigation}
      <div className="settings-layout__content">{children}</div>
    </section>
  );
}
