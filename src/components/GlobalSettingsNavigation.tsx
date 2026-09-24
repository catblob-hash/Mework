import {
  ChartColumn,
  Cloud,
  Command,
  Download,
  Palette,
  Scale,
  Search,
  Terminal
} from "lucide-react";
import { Fragment } from "react";
import { useI18n } from "../i18n";
import type { SettingsView } from "../types";

/**
 * Fixed sidebar information architecture: Providers, Preferences, Efficiency,
 * and System. Group titles are non-interactive, and the order is not
 * data-sorted.
 *
 * Skills and MCP servers have no page here: they are configured on disk, and the
 * conversation-settings pane is where a conversation picks from what was found.
 */
type NavigationGroupId = "providers" | "preferences" | "efficiency" | "system";

const globalSettingsNavigationGroups: Array<{
  id: NavigationGroupId;
  items: Array<{ id: SettingsView; icon: typeof Palette }>;
}> = [
  {
    id: "providers",
    items: [
      { id: "providers", icon: Cloud },
      { id: "search_providers", icon: Search },
      { id: "decision_providers", icon: Scale }
    ]
  },
  {
    id: "preferences",
    items: [
      { id: "appearance", icon: Palette }
    ]
  },
  {
    id: "efficiency",
    items: [
      { id: "shortcuts", icon: Command },
      { id: "usage", icon: ChartColumn }
    ]
  },
  {
    id: "system",
    items: [
      { id: "dependencies", icon: Terminal },
      { id: "updates", icon: Download }
    ]
  }
];

export function GlobalSettingsNavigation({
  view,
  onSelect
}: {
  view: SettingsView;
  onSelect: (view: SettingsView) => void;
}) {
  const { t } = useI18n();
  const labels: Partial<Record<SettingsView, string>> = {
    appearance: t("外观", "Appearance"),
    providers: t("模型提供商", "Model providers"),
    search_providers: t("搜索提供商", "Search providers"),
    decision_providers: t("决策模型提供商", "Decision model providers"),
    shortcuts: t("快捷键", "Keyboard shortcuts"),
    usage: t("用量统计", "Usage statistics"),
    dependencies: t("环境依赖", "Dependencies"),
    updates: t("版本更新", "Updates")
  };
  const groupTitles: Record<NavigationGroupId, string> = {
    providers: t("提供商", "Providers"),
    preferences: t("偏好", "Preferences"),
    efficiency: t("效率", "Efficiency"),
    system: t("系统", "System")
  };
  return (
    <nav className="settings-nav" aria-label={t("全局设置分类", "Global settings categories")}>
      {globalSettingsNavigationGroups.map((group) => (
        <Fragment key={group.id}>
          {groupTitles[group.id] && (
            <div className="settings-nav__group-title">{groupTitles[group.id]}</div>
          )}
          {group.items.map((item) => {
            const Icon = item.icon;
            return (
              <button
                type="button"
                key={item.id}
                className={view === item.id ? "settings-nav__item settings-nav__item--active" : "settings-nav__item"}
                onClick={() => onSelect(item.id)}
              >
                <Icon size={16} /><span>{labels[item.id]}</span>
              </button>
            );
          })}
        </Fragment>
      ))}
    </nav>
  );
}
