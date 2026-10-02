import { History, ListChecks } from "lucide-react";
import { useI18n } from "../i18n";
import type { TasksPaneTab } from "../lib/sidePanes";
import { PageTabs } from "./PageTabs";

export interface TasksPaneTabBarProps {
  activeTab: TasksPaneTab;
  onSelect: (tab: TasksPaneTab) => void;
}

/** The label each of the tasks pane's tabs goes by, which also names the pane while it is shown. */
export function tasksPaneTabLabel(tab: TasksPaneTab, t: ReturnType<typeof useI18n>["t"]): string {
  return tab === "tasks" ? t("任务", "Tasks") : t("历史记录", "History");
}

/**
 * The tasks pane's title bar: its two pages as fixed tabs, tasks on the left. Neither closes nor
 * moves — the pane's × closes the pane — so the strip offers selection and nothing else.
 */
export function TasksPaneTabBar({ activeTab, onSelect }: TasksPaneTabBarProps) {
  const { t } = useI18n();
  return (
    <PageTabs
      tabs={[
        { id: "tasks", label: tasksPaneTabLabel("tasks", t), icon: <ListChecks size={12} aria-hidden="true" /> },
        { id: "history", label: tasksPaneTabLabel("history", t), icon: <History size={12} aria-hidden="true" /> }
      ]}
      activeId={activeTab}
      ariaLabel={t("任务面板标签", "Tasks pane tabs")}
      moreLabel={t("更多页面", "More pages")}
      onSelect={(id) => onSelect(id as TasksPaneTab)}
    />
  );
}
