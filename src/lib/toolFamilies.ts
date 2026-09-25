import type { RememberedToolFamilies } from "../types";

/**
 * The tool-list fields one picker edit writes together: which tools are on,
 * and the rows each switched-off tool family had on. `rememberedToolFamilies`
 * is absent where the owner keeps none of its own.
 */
export interface ToolListSettings {
  enabledTools: string[];
  rememberedToolFamilies?: RememberedToolFamilies;
}

/** The tool families a conversation can remember rows for. Mirrors `ToolFamilyId`. */
const TOOL_FAMILY_IDS: ReadonlySet<string> = new Set(["files", "shell", "preview"]);

/** Keeps the families this build has, each with its row names once and bounded. */
export function normalizeRememberedToolFamilies(value: unknown): RememberedToolFamilies {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const remembered: RememberedToolFamilies = {};
  for (const [family, rows] of Object.entries(value as Record<string, unknown>)) {
    if (!TOOL_FAMILY_IDS.has(family) || !Array.isArray(rows)) continue;
    const names = [...new Set(rows.filter((row): row is string => (
      typeof row === "string" && row.length > 0 && row.length <= 128
    )))].slice(0, 64);
    if (names.length) remembered[family] = names;
  }
  return remembered;
}

/** `settings` with `removed` switched off. */
export function switchToolsOff(settings: ToolListSettings, removed: ReadonlySet<string>): ToolListSettings {
  return { ...settings, enabledTools: settings.enabledTools.filter((tool) => !removed.has(tool)) };
}

/** `settings` with each of `rows` switched on; a row that is already on keeps its place. */
export function switchToolsOn(settings: ToolListSettings, rows: readonly string[]): ToolListSettings {
  return { ...settings, enabledTools: Array.from(new Set([...settings.enabledTools, ...rows])) };
}
