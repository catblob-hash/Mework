import { ChevronDown, Plus, X } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import { useI18n } from "../i18n";
import { IconButton } from "./Common";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem } from "./PopoverMenu";
import "./PreviewPageTabs.css";

export interface PreviewPageTabDescriptor {
  /** The page's native browser session. */
  id: string;
  /** Already resolved: the page's title, its address, or what its start page is for. */
  label: string;
  /** Hover text: the full address, or the workspace a start page belongs to. */
  title?: string;
  /** Drawn before the label — the machine a page on another computer is served from. */
  icon?: ReactNode;
  /**
   * The workspace number, shown only once the conversation has more than one — the same small
   * index the composer's workspace chip carries, so a page reads as belonging to that chip.
   */
  badge?: string;
}

export interface PreviewPageTabsProps {
  tabs: PreviewPageTabDescriptor[];
  activeId: string | null;
  /** Pages being torn down; their close control is spent. */
  closingIds?: ReadonlySet<string>;
  onSelect: (sessionId: string) => void;
  onClose: (sessionId: string) => void;
  /**
   * What `+` does. A function opens a page straight away — the conversation has one workspace, so
   * there is nothing to ask. A list of rows turns `+` into a menu of the workspaces a page can be
   * opened for, the same list the top bar's preview button shows minus its pane row.
   */
  add: (() => void) | PopoverMenuItem[];
}

/**
 * The preview pane's title bar: a tab per page, an overflow selector once they stop fitting, and
 * the control that opens another page.
 *
 * A page is one native browser session. Every workspace of the conversation can have pages of
 * its own — a start page listing its `.mework/launch.json`, or the server it is showing — so the
 * strip is how a conversation that works in several places keeps a preview of each.
 */
export function PreviewPageTabs({
  tabs,
  activeId,
  closingIds,
  onSelect,
  onClose,
  add
}: PreviewPageTabsProps) {
  const { t } = useI18n();
  const stripRef = useRef<HTMLDivElement>(null);
  const [overflowing, setOverflowing] = useState(false);

  // Measured on the names, not only the box: the strip's own width does not change when the tabs
  // inside it stop fitting.
  const tabKey = tabs.map((tab) => `${tab.id} ${tab.label} ${tab.badge ?? ""}`).join("");
  useLayoutEffect(() => {
    const strip = stripRef.current;
    if (!strip) return;
    const measure = () => setOverflowing(strip.scrollWidth - strip.clientWidth > 1);
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(strip);
    return () => observer?.disconnect();
  }, [tabKey]);

  useEffect(() => {
    stripRef.current
      ?.querySelector(`[data-preview-page="${CSS.escape(activeId ?? "")}"]`)
      ?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [activeId]);

  const moveFocus = (event: ReactKeyboardEvent<HTMLButtonElement>, index: number) => {
    const delta = event.key === "ArrowLeft" ? -1 : event.key === "ArrowRight" ? 1 : 0;
    if (delta === 0) return;
    event.preventDefault();
    const next = tabs[(index + delta + tabs.length) % tabs.length];
    if (next) onSelect(next.id);
  };

  const addLabel = t("新建预览页面", "New preview page");
  const moreLabel = t("更多页面", "More pages");

  return (
    <div className="preview-pages">
      <div className="preview-pages__strip" ref={stripRef} role="tablist" aria-label={t("预览页面", "Preview pages")}>
        {tabs.map((tab, index) => {
          const active = tab.id === activeId;
          return (
            <div
              key={tab.id}
              role="presentation"
              className={`preview-page-tab${active ? " preview-page-tab--active" : ""}`}
              data-preview-page={tab.id}
            >
              <button
                type="button"
                role="tab"
                className="preview-page-tab__label"
                aria-selected={active}
                tabIndex={active ? 0 : -1}
                title={tab.title ?? tab.label}
                onClick={() => onSelect(tab.id)}
                onKeyDown={(event) => moveFocus(event, index)}
              >
                {tab.icon && <span className="preview-page-tab__icon" aria-hidden="true">{tab.icon}</span>}
                {tab.badge && <span className="preview-page-tab__badge" aria-hidden="true">{tab.badge}</span>}
                <span className="preview-page-tab__text">{tab.label}</span>
              </button>
              <IconButton
                className="preview-page-tab__close"
                label={t("关闭页面 {name}", "Close page {name}", { name: tab.label })}
                disabled={closingIds?.has(tab.id)}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => onClose(tab.id)}
              >
                <X size={11} aria-hidden="true" />
              </IconButton>
            </div>
          );
        })}
      </div>
      {overflowing && tabs.length > 0 && (
        <PopoverMenu
          rootClassName="preview-pages__overflow"
          triggerClassName="icon-button preview-pages__overflow-trigger"
          trigger={<ChevronDown size={13} aria-hidden="true" />}
          triggerLabel={moreLabel}
          menuLabel={moreLabel}
          align="end"
          dense
          sections={[{
            id: "pages",
            items: tabs.map((tab) => ({
              id: tab.id,
              label: tab.badge ? `${tab.badge} · ${tab.label}` : tab.label,
              checked: tab.id === activeId,
              onSelect: () => onSelect(tab.id)
            }))
          }]}
        />
      )}
      {typeof add === "function" ? (
        <IconButton className="preview-pages__add" label={addLabel} onClick={add}>
          <Plus size={14} aria-hidden="true" />
        </IconButton>
      ) : (
        <PopoverMenu
          rootClassName="preview-pages__add-menu"
          triggerClassName="icon-button preview-pages__add"
          trigger={<Plus size={14} aria-hidden="true" />}
          triggerLabel={addLabel}
          menuLabel={t("为哪个工作区打开预览", "Open a preview for which workspace")}
          align="end"
          dense
          menuWidth={240}
          sections={[{
            id: "workspaces",
            label: t("在哪个工作区打开", "Open in which workspace"),
            items: add
          }]}
        />
      )}
    </div>
  );
}
