import { type TranslationFunction, useI18n } from "../i18n";
import type { ResourceDescriptor } from "../types";
import { CheckRow } from "./Common";

/** Direct multiselect list for capability-catalog resources, shared by skills, MCP, and hooks.
 *
 * Selections retain dangling resource IDs so temporarily unavailable resources can
 * be explicitly deselected rather than silently discarded. */
export function CapabilityResourceList({
  resources,
  selectedIds,
  onChange,
  emptyText
}: {
  resources: ResourceDescriptor[];
  selectedIds: string[];
  onChange: (ids: string[]) => void;
  emptyText: string;
}) {
  const { t } = useI18n();
  const knownIds = new Set(resources.map((resource) => resource.id));
  const danglingIds = selectedIds.filter((id) => !knownIds.has(id));
  const toggle = (id: string, checked: boolean) => {
    onChange(checked
      ? [...selectedIds.filter((existing) => existing !== id), id]
      : selectedIds.filter((existing) => existing !== id));
  };
  return (
    <div className="preset-choice-list">
      {resources.map((resource) => (
        <CheckRow
          key={resource.id}
          checked={selectedIds.includes(resource.id)}
          onChange={(checked) => toggle(resource.id, checked)}
          title={resource.name}
          description={`${resource.description}${!resource.available && selectedIds.includes(resource.id) ? ` · ${t("已选择，当前不会生效", "Selected, currently inactive")}` : ""}`}
          badge={resource.source === "builtin"
            ? t("内置", "Built in")
            : resource.available
              ? resource.source === "workspace" ? t("工作区", "Workspace") : t("用户", "User")
              : t("不可用", "Unavailable")}
        />
      ))}
      {danglingIds.map((id) => (
        <CheckRow
          key={id}
          checked
          onChange={(checked) => toggle(id, checked)}
          title={id}
          description={t("目录中已不存在；运行时忽略，取消勾选可移除", "No longer in the catalog. Ignored at run time; uncheck to remove.")}
          badge={t("悬空", "Dangling")}
        />
      ))}
      {!resources.length && !danglingIds.length && <p className="preset-empty">{emptyText}</p>}
    </div>
  );
}

/** Id of the built-in English profile; selecting nothing means selecting it. */
export const BUILTIN_EN_US_TOOL_DESCRIPTION_ID = "tooldesc_builtin_en_us";
export const BUILTIN_ZH_CN_TOOL_DESCRIPTION_ID = "tooldesc_builtin_zh_cn";

/** Display title of a prompt profile: the two built-ins are localized here
 * because the host names them in one language; files keep their own name. */
export function toolDescriptionProfileTitle(
  resource: ResourceDescriptor | undefined,
  t: TranslationFunction
): string | undefined {
  if (!resource) return undefined;
  if (resource.id === BUILTIN_EN_US_TOOL_DESCRIPTION_ID) return t("Mework 内置（英文）", "Mework built-in (English)");
  if (resource.id === BUILTIN_ZH_CN_TOOL_DESCRIPTION_ID) return t("Mework 内置（中文）", "Mework built-in (Chinese)");
  return resource.name;
}

/** Single-select list of prompt profiles (tool-description files).
 *
 * The two built-in profiles always lead the list and cannot be removed; the
 * English one is what a conversation renders with when it selects nothing, so
 * it shows as selected for a `null` id. Files discovered on disk follow. The
 * app discovers and selects files but never edits them. */
export function ToolDescriptionSetList({
  resources,
  selectedId,
  onChange
}: {
  resources: ResourceDescriptor[];
  selectedId: string | null;
  onChange: (id: string | null) => void;
}) {
  const { t } = useI18n();
  const known = new Set(resources.map((resource) => resource.id));
  const dangling = selectedId && !known.has(selectedId) ? selectedId : null;
  const builtinDescription = (resource: ResourceDescriptor): string | null => {
    if (resource.id === BUILTIN_EN_US_TOOL_DESCRIPTION_ID) {
      return t("默认：英文提示词与工具描述，不可删除", "Default: English prompts and tool descriptions, cannot be removed");
    }
    if (resource.id === BUILTIN_ZH_CN_TOOL_DESCRIPTION_ID) {
      return t("中文提示词与工具描述，不可删除", "Chinese prompts and tool descriptions, cannot be removed");
    }
    return null;
  };
  const isChecked = (resource: ResourceDescriptor) =>
    selectedId === resource.id
    || (selectedId === null && resource.id === BUILTIN_EN_US_TOOL_DESCRIPTION_ID);
  return (
    <div className="preset-choice-list">
      {resources.map((resource) => {
        const builtin = builtinDescription(resource);
        return (
          <CheckRow
            key={resource.id}
            checked={isChecked(resource)}
            onChange={(checked) => onChange(checked ? resource.id : null)}
            title={toolDescriptionProfileTitle(resource, t) ?? resource.name}
            description={`${builtin ?? resource.description}${!resource.available && selectedId === resource.id ? ` · ${t("已选择，当前不会生效", "Selected, currently inactive")}` : ""}`}
            badge={resource.source === "builtin"
              ? t("内置", "Built in")
              : resource.available
                ? resource.source === "workspace" ? t("工作区", "Workspace") : t("用户", "User")
                : t("不可用", "Unavailable")}
          />
        );
      })}
      {dangling && (
        <CheckRow
          checked
          onChange={() => onChange(null)}
          title={dangling}
          description={t("目录中已不存在；运行时使用内置英文提示词，取消勾选可移除", "No longer in the catalog. The built-in English prompts are used at run time; uncheck to remove.")}
          badge={t("悬空", "Dangling")}
        />
      )}
      <p className="preset-empty">{t(
        "自定义文件放在 ~/.mework/tool-descriptions/*.json 或工作区的 .mework/tool-descriptions/ 下",
        "Put custom files under ~/.mework/tool-descriptions/*.json or the workspace's .mework/tool-descriptions/"
      )}</p>
    </div>
  );
}
