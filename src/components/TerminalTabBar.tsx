import { Plus } from "lucide-react";
import { useCallback, useState } from "react";
import { useI18n } from "../i18n";
import { PageTabs } from "./PageTabs";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuSection } from "./PopoverMenu";

export interface TerminalTabDescriptor {
  id: string;
  /** Already resolved: the user's name, or the derived one. */
  label: string;
}

export interface TerminalTabBarProps {
  tabs: TerminalTabDescriptor[];
  activeId: string | null;
  /** The DOM id of the panel a tab controls, for `aria-controls`. */
  panelId: (terminalId: string) => string;
  /** Tabs whose shell is being killed; their close control is spent. */
  closingIds?: ReadonlySet<string>;
  onSelect: (terminalId: string) => void;
  onClose: (terminalId: string) => void;
  onRename: (terminalId: string, name: string) => void;
  /** Makes the tab order the user's; receives every terminal id in the new order. */
  onReorder?: (terminalIds: string[]) => void;
  /**
   * The menu `+` opens: where a new terminal can start — the same choice the top bar's terminal
   * button offers, minus its pane row. `flyout` opens a workspace's shells beside it.
   */
  add: { sections: PopoverMenuSection[]; submenu?: "inline" | "flyout" };
}

/**
 * The terminal pane's title bar: a tab per shell on the shared page strip, and the menu that opens
 * another one. It takes the pane's `header` slot, so the pane draws no title of its own and the bar
 * is the only chrome above the terminal — the reference shell puts nothing else there. A double
 * click renames a shell in place.
 */
export function TerminalTabBar({
  tabs,
  activeId,
  panelId,
  closingIds,
  onSelect,
  onClose,
  onRename,
  onReorder,
  add
}: TerminalTabBarProps) {
  const { t } = useI18n();
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");

  const commitRename = useCallback(() => {
    if (renamingId === null) return;
    onRename(renamingId, draft);
    setRenamingId(null);
  }, [draft, onRename, renamingId]);

  const startRename = (terminalId: string) => {
    const tab = tabs.find((candidate) => candidate.id === terminalId);
    if (!tab) return;
    setDraft(tab.label);
    setRenamingId(tab.id);
  };

  const addLabel = t("新建终端", "New terminal");

  return (
    <PageTabs
      tabs={tabs.map((tab) => ({ id: tab.id, label: tab.label, closeDisabled: closingIds?.has(tab.id) }))}
      activeId={activeId}
      ariaLabel={t("终端标签", "Terminal tabs")}
      moreLabel={t("更多终端", "More terminals")}
      panelId={panelId}
      onSelect={onSelect}
      onClose={onClose}
      closeLabel={() => t("关闭终端", "Close terminal")}
      onReorder={onReorder}
      onTabDoubleClick={startRename}
      renderEditor={(tab) => (tab.id !== renamingId ? null : (
        <input
          className="page-tab__editor"
          aria-label={t("重命名终端", "Rename terminal")}
          value={draft}
          // biome-ignore lint/a11y/noAutofocus: the field exists only because a double click just asked to type into it.
          autoFocus
          onChange={(event) => setDraft(event.target.value)}
          onBlur={commitRename}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              commitRename();
            } else if (event.key === "Escape") {
              event.preventDefault();
              setRenamingId(null);
            }
          }}
        />
      ))}
      trailing={(
        <PopoverMenu
          triggerClassName="icon-button"
          trigger={<Plus size={14} aria-hidden="true" />}
          triggerLabel={addLabel}
          menuLabel={addLabel}
          align="end"
          dense
          submenu={add.submenu}
          sections={add.sections}
        />
      )}
    />
  );
}
