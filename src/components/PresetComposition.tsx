import { type TranslationFunction, useI18n } from "../i18n";
import type { ResourceDescriptor } from "../types";

/** Id of the built-in profile; selecting nothing means selecting it. */
const BUILTIN_TOOL_DESCRIPTION_ID = "tooldesc_builtin_en_us";

/** Display title of a prompt profile: the built-in is localized here because
 * the host names it in one language; files keep their own name. */
function toolDescriptionProfileTitle(
  resource: ResourceDescriptor | undefined,
  t: TranslationFunction
): string | undefined {
  if (!resource) return undefined;
  if (resource.id === BUILTIN_TOOL_DESCRIPTION_ID) return t("Mework 内置", "Mework built-in");
  return resource.name;
}

/** Row-shaped single-select over the prompt profiles the catalog discovered.
 *
 * Conversations pick a profile and nothing else, so they get the provider-row
 * shape rather than a list of checkboxes. Selecting nothing still means the
 * built-in profile, which is why that entry is what a `null` id shows. */
export function ToolDescriptionSelectRow({
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
  return (
    <div className="tool-toggle-row">
      <span><strong>{t("工具描述", "Tool descriptions")}</strong><small>{t(
        "决定系统提示词与工具描述用哪一份。自定义文件放在 ~/.mework/tool-descriptions/*.json 或工作区的 .mework/tool-descriptions/ 下。",
        "Picks which prompt and tool-description profile this conversation renders with. Put custom files under ~/.mework/tool-descriptions/*.json or the workspace's .mework/tool-descriptions/."
      )}</small></span>
      <select
        className="input"
        aria-label={t("工具描述", "Tool descriptions")}
        value={dangling ?? selectedId ?? BUILTIN_TOOL_DESCRIPTION_ID}
        onChange={(event) => onChange(event.target.value)}
      >
        {resources.map((resource) => {
          const title = toolDescriptionProfileTitle(resource, t) ?? resource.name;
          return (
            <option key={resource.id} value={resource.id}>
              {resource.available
                ? title
                : t("{name}（不可用）", "{name} (unavailable)", { name: title })}
            </option>
          );
        })}
        {/* A file that has left the catalog leaves a dangling id. Showing it
            keeps the row explainable and lets the user pick their way out,
            rather than reading as the built-in the host falls back to. */}
        {dangling && <option value={dangling}>{t(
          "{id}（目录中已不存在）",
          "{id} (no longer in the catalog)",
          { id: dangling }
        )}</option>}
      </select>
    </div>
  );
}
