import { Plus } from "lucide-react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import { IconButton } from "./Common";
import { PageTabs } from "./PageTabs";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem } from "./PopoverMenu";

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
  /** Makes the page order the user's; receives every session id in the new order. */
  onReorder?: (sessionIds: string[]) => void;
  /**
   * What `+` does. A function opens a page straight away — the conversation has one workspace, so
   * there is nothing to ask. A list of rows turns `+` into a menu of the workspaces a page can be
   * opened for, the same list the top bar's preview button shows minus its pane row.
   */
  add: (() => void) | PopoverMenuItem[];
}

/**
 * The preview pane's title bar: a tab per page on the shared page strip, and the control that
 * opens another page.
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
  onReorder,
  add
}: PreviewPageTabsProps) {
  const { t } = useI18n();
  const addLabel = t("新建预览页面", "New preview page");

  return (
    <PageTabs
      tabs={tabs.map((tab) => ({ ...tab, closeDisabled: closingIds?.has(tab.id) }))}
      activeId={activeId}
      ariaLabel={t("预览页面", "Preview pages")}
      moreLabel={t("更多页面", "More pages")}
      onSelect={onSelect}
      onClose={onClose}
      closeLabel={(tab) => t("关闭页面 {name}", "Close page {name}", { name: tab.label })}
      onReorder={onReorder}
      trailing={typeof add === "function" ? (
        <IconButton label={addLabel} onClick={add}>
          <Plus size={14} aria-hidden="true" />
        </IconButton>
      ) : (
        <PopoverMenu
          triggerClassName="icon-button"
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
    />
  );
}
