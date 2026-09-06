import {
  ChevronRight,
  Cloud,
  Command,
  Download,
  MessageSquareText,
  Palette,
  Plug,
  Search,
  Terminal,
  Wrench
} from "lucide-react";
import { Fragment } from "react";
import { useI18n } from "../i18n";
import type { SettingsView } from "../types";

/**
 * Fixed sidebar information architecture: Providers, Tools, Preferences,
 * Efficiency, and System. Group titles are non-interactive, and the order is
 * not data-sorted.
 */
type NavigationGroupId = "providers" | "tools" | "preferences" | "efficiency" | "system";

export const globalSettingsNavigationGroups: Array<{
  id: NavigationGroupId;
  items: Array<{ id: SettingsView; icon: typeof Palette }>;
}> = [
  {
    id: "providers",
    items: [
      { id: "providers", icon: Cloud },
      { id: "search_providers", icon: Search }
    ]
  },
  {
    id: "tools",
    items: [
      { id: "mcp", icon: Plug },
      { id: "skills", icon: Wrench }
    ]
  },
  {
    id: "preferences",
    items: [
      { id: "appearance", icon: Palette },
      { id: "conversation_presets", icon: MessageSquareText }
    ]
  },
  {
    id: "efficiency",
    items: [{ id: "shortcuts", icon: Command }]
  },
  {
    id: "system",
    items: [
      { id: "dependencies", icon: Terminal },
      { id: "updates", icon: Download }
    ]
  }
];

/** Flat order for callers that enumerate items or navigate to the next or previous item. */
export const globalSettingsNavigationItems = globalSettingsNavigationGroups.flatMap(
  (group) => group.items
);

export function GlobalSettingsNavigation({
  view,
  onSelect,
  className = "settings-nav"
}: {
  view: SettingsView;
  onSelect: (view: SettingsView) => void;
  className?: string;
}) {
  const { t } = useI18n();
  const labels: Partial<Record<SettingsView, string>> = {
    appearance: t("外观", "Appearance"),
    conversation_presets: t("对话预设", "Conversation presets"),
    providers: t("模型提供商", "Model providers"),
    search_providers: t("搜索提供商", "Search providers"),
    mcp: "MCP",
    skills: t("技能", "Skills"),
    shortcuts: t("快捷键", "Keyboard shortcuts"),
    dependencies: t("环境依赖", "Dependencies"),
    updates: t("版本更新", "Updates")
  };
  const groupTitles: Record<NavigationGroupId, string> = {
    providers: t("提供商", "Providers"),
    tools: t("工具", "Tools"),
    preferences: t("偏好", "Preferences"),
    efficiency: t("效率", "Efficiency"),
    system: t("系统", "System")
  };
  return (
    <nav className={className} aria-label={t("全局设置分类", "Global settings categories")}>
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
                <Icon size={16} /><span>{labels[item.id]}</span><ChevronRight size={14} />
              </button>
            );
          })}
        </Fragment>
      ))}
    </nav>
  );
}
