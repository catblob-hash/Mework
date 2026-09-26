import {
  BrainCircuit,
  ChevronRight,
  Command,
  Database,
  FileCode2,
  Globe2,
  Lock,
  Minus,
  Plug,
  Plus
} from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import {
  isHostDerivedToolName,
  isPreviewLifecycleToolName,
  withPreviewLifecycleTools
} from "../lib/taskTools";
import type { ToolDescriptor } from "../types";
import { ToolDocsLink } from "./DocsLink";

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
  if (category === "web") return t("预览", "Preview");
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
  lockedTools = []
}: {
  tools: ToolDescriptor[];
  enabledTools: string[];
  onChange: (enabledTools: string[]) => void;
  expansionKey: string;
  /**
   * Tools a run has already put in front of the model. They leave the picker for
   * a single collapsed bar, so what remains below is exactly the surface still
   * free to move.
   */
  lockedTools?: readonly string[];
}) {
  const { t } = useI18n();
  const instanceId = useId().replace(/:/g, "");
  const availableNames = useMemo(() => new Set(tools.map((tool) => tool.name)), [tools]);
  const lockedNames = useMemo(() => new Set(lockedTools), [lockedTools]);
  /** Every list the picker writes keeps the preview lifecycle tools in step with the rows it shows. */
  const write = (next: string[]) => onChange(withPreviewLifecycleTools(next, availableNames, lockedNames));
  const groupedTools = useMemo(() => {
    const pickable = uniqueTools(tools).filter(
      // Memory, task-runtime, and skill tools are host-derived and cannot be
      // individually toggled. Category filtering is not a substitute for
      // `isHostDerivedToolName`. The preview lifecycle tools follow the other
      // preview tools, so they have no row either.
      (tool) => tool.category !== "memory"
        && !isHostDerivedToolName(tool.name)
        && !isPreviewLifecycleToolName(tool.name)
    );
    return (Object.keys(groupMeta) as ToolCategory[])
      .filter((category) => category !== "memory")
      .map((category) => ({
        category,
        // Nested tools render below their parent and do not occupy top-level rows.
        tools: pickable.filter(
          (tool) => tool.category === category && !NESTED_TOOL_NAMES.has(tool.name)
        ),
        allTools: pickable.filter((tool) => tool.category === category)
      })).filter((group) => group.tools.length > 0);
  }, [tools]);
  /** Locked rows keep catalog order so the bar reads like the picker it left. */
  const lockedRows = useMemo(
    () => groupedTools.flatMap((group) => group.allTools.filter((tool) => lockedNames.has(tool.name))),
    [groupedTools, lockedNames]
  );
  const [collapsedGroups, setCollapsedGroups] = useState<Set<ToolCategory>>(() => new Set());
  const [lockedExpanded, setLockedExpanded] = useState(false);
  const previousExpansionKey = useRef(expansionKey);

  // Collapse state changes only by user action so disabling the final tool does
  // not hide the group needed to re-enable it.
  useEffect(() => {
    if (previousExpansionKey.current === expansionKey) return;
    previousExpansionKey.current = expansionKey;
    setCollapsedGroups(new Set());
    setLockedExpanded(false);
  }, [expansionKey]);

  const toggleTool = (name: string, checked: boolean) => {
    if (checked) {
      write(Array.from(new Set([...enabledTools, name])));
      return;
    }
    // Disabling a parent also removes dependents so no unreachable toggle or
    // inaccurate count remains. Locked names survive either way: the model has
    // already been handed them and the transcript may already call them.
    const removed = new Set([name, ...(NESTED_TOOLS[name] ?? [])].filter((tool) => !lockedNames.has(tool)));
    if (!removed.size) return;
    write(enabledTools.filter((tool) => !removed.has(tool)));
  };

  /**
   * One group's whole membership, on or off.
   *
   * It acts on the rows the group actually offers — locked names are left
   * alone in both directions, because they are spent rather than selected —
   * and it includes the dependents that only ever render under a parent, so
   * "all of this group" means the same thing the heading's count does.
   */
  const setGroupEnabled = (category: ToolCategory, enabled: boolean) => {
    const group = groupedTools.find((candidate) => candidate.category === category);
    if (!group) return;
    const names = group.allTools.filter((tool) => !lockedNames.has(tool.name)).map((tool) => tool.name);
    if (enabled) {
      // Turning a group on is also a way of asking to see it: a collapsed
      // group would otherwise report a new count with nothing to show for it.
      setCollapsedGroups((current) => {
        if (!current.has(category)) return current;
        const next = new Set(current);
        next.delete(category);
        return next;
      });
      write(Array.from(new Set([...enabledTools, ...names])));
      return;
    }
    // Turning one off has no second job, so it leaves the disclosure alone.
    const removed = new Set(names);
    if (!removed.size) return;
    write(enabledTools.filter((tool) => !removed.has(tool)));
  };

  const lockedRegionId = `tool-lock-${instanceId}`;
  return (
    <div className="tool-settings-groups">
      {lockedRows.length > 0 && (
        <div className="tool-settings-group tool-settings-group--locked">
          <div className="tool-settings-group__heading">
            <button
              type="button"
              className="tool-settings-group__disclosure"
              aria-expanded={lockedExpanded}
              aria-controls={lockedRegionId}
              onClick={() => setLockedExpanded((current) => !current)}
            >
              <Lock size={14} />
              <span className="tool-settings-group__label">{t("已生效的工具", "Tools already in play")}</span>
              <small>{lockedRows.length}</small>
            </button>
            {/* No bulk pair here: nothing in this group can move. The chevron
                still gets its own grip, so this bar's right edge behaves like
                every other group's. */}
            <button
              type="button"
              className="tool-settings-group__chevron"
              tabIndex={-1}
              aria-hidden="true"
              onClick={() => setLockedExpanded((current) => !current)}
            >
              <ChevronRight className={`disclosure-chevron${lockedExpanded ? " disclosure-chevron--open" : ""}`} size={14} />
            </button>
          </div>
          <div
            id={lockedRegionId}
            className={`collapse-region ${lockedExpanded ? "" : "collapse-region--closed"}`}
            aria-hidden={!lockedExpanded || undefined}
            inert={!lockedExpanded || undefined}
          >
            <div className="collapse-region__inner">
              <p className="tool-lock-note">{t(
                "这些工具已经交给过模型。之后的回合只能在它们之上再加，不能收回。",
                "The model has already been handed these. Later rounds can only add on top of them, never take them back."
              )}</p>
              {lockedRows.map((tool) => (
                <div className="tool-pick-row" key={tool.name}>
                  <ToolDocsLink name={tool.name} label={tool.label} />
                  <div className="tool-toggle-row tool-toggle-row--locked" data-tool-name={tool.name}>
                    <span><strong>{tool.label}</strong></span>
                    <ToolMarks tool={tool} />
                    <Lock className="tool-toggle-row__mark" size={14} aria-hidden="true" />
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      )}
      {groupedTools.map(({ category, tools: groupTools, allTools }) => {
        const meta = groupMeta[category];
        const label = groupLabel(category, t);
        const GroupIcon = meta.icon;
        const expanded = !collapsedGroups.has(category);
        const pickable = allTools.filter((tool) => !lockedNames.has(tool.name));
        if (!pickable.length) return null;
        const enabledCount = pickable.filter((tool) => enabledTools.includes(tool.name)).length;
        const regionId = `tool-group-${instanceId}-${category}`;
        const toggleGroup = () => setCollapsedGroups((current) => {
          const next = new Set(current);
          if (next.has(category)) next.delete(category); else next.add(category);
          return next;
        });
        return (
          <div className="tool-settings-group" data-tool-category={category} key={category}>
            <div className="tool-settings-group__heading">
              <button
                type="button"
                className="tool-settings-group__disclosure"
                aria-label={label}
                aria-expanded={expanded}
                aria-controls={regionId}
                onClick={toggleGroup}
              >
                <GroupIcon size={14} />
                <span className="tool-settings-group__label">{label}</span>
                <small>{enabledCount} / {pickable.length}</small>
              </button>
              {/* The group's own pair, in the column the tool rows' signs will
                  read down from. They sit beside the disclosure rather than
                  inside it because a button cannot hold two more. */}
              <span className="tool-settings-group__bulk">
                <button
                  type="button"
                  className="tool-settings-group__bulk-button"
                  aria-label={t("全选{label}", "Select all in {label}", { label })}
                  onClick={() => setGroupEnabled(category, true)}
                >
                  <Plus size={13} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="tool-settings-group__bulk-button"
                  aria-label={t("全不选{label}", "Clear all in {label}", { label })}
                  disabled={enabledCount === 0}
                  onClick={() => setGroupEnabled(category, false)}
                >
                  <Minus size={13} aria-hidden="true" />
                </button>
              </span>
              {/* A second way to work the same disclosure, for the chevron
                  itself — the part of a heading people aim at. The labelled
                  button above is the one assistive technology announces. */}
              <button
                type="button"
                className="tool-settings-group__chevron"
                tabIndex={-1}
                aria-hidden="true"
                onClick={toggleGroup}
              >
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
                    lockedNames={lockedNames}
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

/** The mark a tool row carries before its trailing sign when its calls are reviewed. */
function ToolMarks({ tool }: { tool: ToolDescriptor }) {
  const { t } = useI18n();
  return tool.dangerous ? <em>{t("需审查", "Reviewed")}</em> : null;
}

/** A tool-toggle row. Nested tools have no disclosure: their presence is fully
 * controlled by the parent row, so parent labels stay aligned with other rows. */
function ToolToggle({
  tool,
  enabledTools,
  lockedNames,
  onToggle,
  nested
}: {
  tool: ToolDescriptor;
  enabledTools: string[];
  lockedNames: ReadonlySet<string>;
  onToggle: (name: string, checked: boolean) => void;
  nested: ToolDescriptor[];
}) {
  const enabled = enabledTools.includes(tool.name) || lockedNames.has(tool.name);
  // A locked parent has left for the lock bar, but its dependants are separate
  // names and may still be free; keep their slot where the parent's row was.
  const parentLocked = lockedNames.has(tool.name);
  const pickableNested = nested.filter((child) => !lockedNames.has(child.name));
  const showNested = pickableNested.length > 0 && enabled;
  if (parentLocked && !showNested) return null;
  return (
    <>
      {!parentLocked && <ToolPickRow tool={tool} enabled={enabled} onToggle={onToggle} />}
      {showNested && pickableNested.map((child) => (
        <ToolPickRow
          key={child.name}
          tool={child}
          enabled={enabledTools.includes(child.name)}
          onToggle={onToggle}
          nested
        />
      ))}
    </>
  );
}

/** One pickable tool. The row's button is the control: it fills in when the tool
 * is on, and the trailing sign says which way a click moves it. The way in to
 * the tool's own documentation sits outside that button, in the slot the group
 * heading above puts its icon — so a tool's name and its group's name read down
 * one column, and the sign and the group's chevron read down another. */
function ToolPickRow({
  tool,
  enabled,
  onToggle,
  nested = false
}: {
  tool: ToolDescriptor;
  enabled: boolean;
  onToggle: (name: string, checked: boolean) => void;
  nested?: boolean;
}) {
  const { t } = useI18n();
  const Mark = enabled ? Minus : Plus;
  return (
    <div className={`tool-pick-row${nested ? " tool-pick-row--nested" : ""}`}>
      <ToolDocsLink name={tool.name} label={tool.label} />
      <button
        type="button"
        className={`tool-toggle-row tool-toggle-row--pick${enabled ? " tool-toggle-row--on" : ""}${nested ? " tool-toggle-row--nested" : ""}`}
        data-tool-name={tool.name}
        aria-pressed={enabled}
        aria-label={enabled
          ? t("{label}已启用", "{label} enabled", { label: tool.label })
          : t("{label}已关闭", "{label} disabled", { label: tool.label })}
        onClick={() => onToggle(tool.name, !enabled)}
      >
        <span><strong>{tool.label}</strong></span>
        <ToolMarks tool={tool} />
        <Mark className="tool-toggle-row__mark" size={14} aria-hidden="true" />
      </button>
    </div>
  );
}
