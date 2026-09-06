import {
  BrainCircuit,
  ChevronRight,
  Command,
  Database,
  FileCode2,
  Globe2,
  Plug
} from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { isHostDerivedToolName } from "../lib/taskTools";
import type { ToolDescriptor } from "../types";
import { Switch } from "./Common";

export type ToolCategory = ToolDescriptor["category"];

/**
 * Single source of truth for tool grouping shared by the enabled-tools and tool-
 * descriptions views. Object key order determines render order.
 */
export const groupMeta = {
  filesystem: { icon: FileCode2 },
  shell: { icon: Command },
  web: { icon: Globe2 },
  orchestration: { icon: BrainCircuit },
  memory: { icon: Database },
  mcp: { icon: Plug }
} as const satisfies Record<ToolCategory, { icon: typeof Globe2 }>;

/** Dependent tools require their parent. They are hidden and cannot be enabled
 * without it; without `agent_spawn`, `send_message` is unreachable. */
const NESTED_TOOLS: Record<string, readonly string[]> = {
  agent_spawn: ["send_message", "followup_task"]
};

const NESTED_TOOL_NAMES: ReadonlySet<string> = new Set(Object.values(NESTED_TOOLS).flat());

type Translate = ReturnType<typeof useI18n>["t"];

/** The fallback is not type-protected. Adding a `ToolCategory` makes `groupMeta`
 * fail through `satisfies`, but this function must be updated explicitly too. */
export function groupLabel(category: ToolCategory, t: Translate): string {
  if (category === "filesystem") return t("文件与搜索", "Files and search");
  if (category === "web") return t("联网", "Web");
  if (category === "memory") return t("长期记忆", "Long-term memory");
  if (category === "mcp") return "MCP";
  if (category === "orchestration") return t("代理编排", "Agent orchestration");
  return "Shell";
}

function uniqueTools(tools: ToolDescriptor[]): ToolDescriptor[] {
  const seen = new Set<string>();
  return tools.filter((tool) => {
    if (seen.has(tool.name)) return false;
    seen.add(tool.name);
    return true;
  });
}

export function ToolSelectionGroups({
  tools,
  enabledTools,
  onChange,
  expansionKey,
  density = "sidebar"
}: {
  tools: ToolDescriptor[];
  enabledTools: string[];
  onChange: (enabledTools: string[]) => void;
  expansionKey: string;
  density?: "sidebar" | "settings" | "column";
}) {
  const { t } = useI18n();
  const instanceId = useId().replace(/:/g, "");
  const groupedTools = useMemo(() => {
    const deduplicated = uniqueTools(tools).filter(
      // Memory, task-runtime, and skill tools are host-derived and cannot be
      // individually toggled. Category filtering is not a substitute for
      // `isHostDerivedToolName`.
      (tool) => tool.category !== "memory" && !isHostDerivedToolName(tool.name)
    );
    return (Object.keys(groupMeta) as ToolCategory[])
      .filter((category) => category !== "memory")
      .map((category) => ({
        category,
        // Nested tools render below their parent and do not occupy top-level rows.
        tools: deduplicated.filter(
          (tool) => tool.category === category && !NESTED_TOOL_NAMES.has(tool.name)
        ),
        allTools: deduplicated.filter((tool) => tool.category === category)
      })).filter((group) => group.tools.length > 0);
  }, [tools]);
  const [collapsedGroups, setCollapsedGroups] = useState<Set<ToolCategory>>(() => new Set());
  const previousExpansionKey = useRef(expansionKey);

  // Collapse state changes only by user action so disabling the final tool does
  // not hide the group needed to re-enable it.
  useEffect(() => {
    if (previousExpansionKey.current === expansionKey) return;
    previousExpansionKey.current = expansionKey;
    setCollapsedGroups(new Set());
  }, [expansionKey]);

  const toggleTool = (name: string, checked: boolean) => {
    if (checked) {
      onChange(Array.from(new Set([...enabledTools, name])));
      return;
    }
    // Disabling a parent also removes dependents so no unreachable toggle or
    // inaccurate count remains.
    const removed = new Set([name, ...(NESTED_TOOLS[name] ?? [])]);
    onChange(enabledTools.filter((tool) => !removed.has(tool)));
  };

  return (
    <div className={`tool-settings-groups tool-settings-groups--${density}`}>
      {groupedTools.map(({ category, tools: groupTools, allTools }) => {
        const meta = groupMeta[category];
        const label = groupLabel(category, t);
        const GroupIcon = meta.icon;
        const expanded = !collapsedGroups.has(category);
        const enabledCount = allTools.filter((tool) => enabledTools.includes(tool.name)).length;
        const regionId = `tool-group-${instanceId}-${category}`;
        return (
          <div className="tool-settings-group" data-tool-category={category} key={category}>
            <div className="tool-settings-group__heading">
              <button
                type="button"
                className="tool-settings-group__disclosure"
                aria-label={label}
                aria-expanded={expanded}
                aria-controls={regionId}
                onClick={() => setCollapsedGroups((current) => {
                  const next = new Set(current);
                  if (next.has(category)) next.delete(category); else next.add(category);
                  return next;
                })}
              >
                <GroupIcon size={14} />
                <span className="tool-settings-group__label">{label}</span>
                <small>{enabledCount} / {allTools.length}</small>
                <ChevronRight className={`disclosure-chevron${expanded ? " disclosure-chevron--open" : ""}`} size={14} />
              </button>
            </div>
            <div
              id={regionId}
              className={`collapse-region ${expanded ? "" : "collapse-region--closed"}`}
              aria-hidden={!expanded || undefined}
              inert={!expanded || undefined}
            >
              <div className="collapse-region__inner">
                {groupTools.map((tool) => (
                  <ToolToggle
                    key={tool.name}
                    tool={tool}
                    enabledTools={enabledTools}
                    onToggle={toggleTool}
                    nested={allTools.filter((candidate) =>
                      (NESTED_TOOLS[tool.name] ?? []).includes(candidate.name))}
                  />
                ))}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

/** A tool-toggle row. Nested tools have no disclosure: their presence is fully
 * controlled by the parent switch, so parent labels stay aligned with other rows. */
function ToolToggle({
  tool,
  enabledTools,
  onToggle,
  nested
}: {
  tool: ToolDescriptor;
  enabledTools: string[];
  onToggle: (name: string, checked: boolean) => void;
  nested: ToolDescriptor[];
}) {
  const { t } = useI18n();
  const enabled = enabledTools.includes(tool.name);
  const showNested = nested.length > 0 && enabled;
  const toggleLabel = enabled
    ? t("{label}已启用", "{label} enabled", { label: tool.label })
    : t("{label}已关闭", "{label} disabled", { label: tool.label });
  return (
    <>
      <div className="tool-toggle-row" data-tool-name={tool.name}>
        <span><strong>{tool.label}</strong></span>
        {tool.dangerous && <em>{t("需审查", "Reviewed")}</em>}
        <Switch
          checked={enabled}
          onChange={(checked) => onToggle(tool.name, checked)}
          label={toggleLabel}
        />
      </div>
      {showNested && nested.map((child) => (
        <div className="tool-toggle-row tool-toggle-row--nested" data-tool-name={child.name} key={child.name}>
          <span><strong>{child.label}</strong></span>
          {child.dangerous && <em>{t("需审查", "Reviewed")}</em>}
          <Switch
            checked={enabledTools.includes(child.name)}
            onChange={(checked) => onToggle(child.name, checked)}
            label={enabledTools.includes(child.name)
              ? t("{label}已启用", "{label} enabled", { label: child.label })
              : t("{label}已关闭", "{label} disabled", { label: child.label })}
          />
        </div>
      ))}
    </>
  );
}
