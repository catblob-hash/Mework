import {
  BrainCircuit,
  ChevronRight,
  Command,
  Database,
  FileCode2,
  Globe2,
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
import type { LockTone } from "../lib/toolLock";
import type { ToolDescriptor } from "../types";
import { ToolDocsLink } from "./DocsLink";
import { LockMark, lockToneClass, type LockHints } from "./LockTone";

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
  toneOf,
  lockHints
}: {
  tools: ToolDescriptor[];
  enabledTools: string[];
  onChange: (enabledTools: string[]) => void;
  expansionKey: string;
  /**
   * How the conversation's lock draws a row (`lockTone` in `toolLock.ts`). Every
   * row stays where it is: an orange one still moves — the caller warns first —
   * and a gray one does not.
   */
  toneOf?: (name: string, enabled: boolean) => LockTone | null;
  /** What a toned row says about itself, as its tooltip. */
  lockHints?: LockHints;
}) {
  const { t } = useI18n();
  const instanceId = useId().replace(/:/g, "");
  const availableNames = useMemo(() => new Set(tools.map((tool) => tool.name)), [tools]);
  const tone = (name: string) => toneOf?.(name, enabledTools.includes(name)) ?? null;
  const frozen = (name: string) => tone(name) === "hard";
  /** Every list the picker writes keeps the preview lifecycle tools in step with the rows it shows. */
  const write = (next: string[]) => onChange(withPreviewLifecycleTools(next, availableNames));
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
        tools: pickable.filter((tool) => tool.category === category)
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
    if (frozen(name)) return;
    if (checked) {
      write(Array.from(new Set([...enabledTools, name])));
      return;
    }
    write(enabledTools.filter((tool) => tool !== name));
  };

  /**
   * One group's whole membership, on or off.
   *
   * It acts on the rows the group can move — a gray row is left alone in both
   * directions.
   */
  const setGroupEnabled = (category: ToolCategory, enabled: boolean) => {
    const group = groupedTools.find((candidate) => candidate.category === category);
    if (!group) return;
    const names = group.tools.filter((tool) => !frozen(tool.name)).map((tool) => tool.name);
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

  return (
    <div className="tool-settings-groups">
      {groupedTools.map(({ category, tools: groupTools }) => {
        const meta = groupMeta[category];
        const label = groupLabel(category, t);
        const GroupIcon = meta.icon;
        const expanded = !collapsedGroups.has(category);
        const enabledCount = groupTools.filter((tool) => enabledTools.includes(tool.name)).length;
        const movable = groupTools.filter((tool) => !frozen(tool.name));
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
                <small>{enabledCount} / {groupTools.length}</small>
              </button>
              {/* The group's own pair, in the column the tool rows' signs will
                  read down from. They sit beside the disclosure rather than
                  inside it because a button cannot hold two more. */}
              <span className="tool-settings-group__bulk">
                <button
                  type="button"
                  className="tool-settings-group__bulk-button"
                  aria-label={t("全选{label}", "Select all in {label}", { label })}
                  disabled={movable.every((tool) => enabledTools.includes(tool.name))}
                  onClick={() => setGroupEnabled(category, true)}
                >
                  <Plus size={13} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="tool-settings-group__bulk-button"
                  aria-label={t("全不选{label}", "Clear all in {label}", { label })}
                  disabled={!movable.some((tool) => enabledTools.includes(tool.name))}
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
                  <ToolPickRow
                    key={tool.name}
                    tool={tool}
                    enabled={enabledTools.includes(tool.name)}
                    tone={tone(tool.name)}
                    lockHints={lockHints}
                    onToggle={toggleTool}
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

/** One pickable tool. The row's button is the control: it fills in when the tool
 * is on, and the trailing sign says which way a click moves it. The way in to
 * the tool's own documentation sits outside that button, in the slot the group
 * heading above puts its icon — so a tool's name and its group's name read down
 * one column, and the sign and the group's chevron read down another.
 *
 * A row the lock tones keeps its place and trades its sign for a lock: orange
 * still clicks (through the caller's warning), gray does not. */
function ToolPickRow({
  tool,
  enabled,
  tone = null,
  lockHints,
  onToggle
}: {
  tool: ToolDescriptor;
  enabled: boolean;
  tone?: LockTone | null;
  lockHints?: LockHints;
  onToggle: (name: string, checked: boolean) => void;
}) {
  const { t } = useI18n();
  const Mark = enabled ? Minus : Plus;
  return (
    <div className="tool-pick-row">
      <ToolDocsLink name={tool.name} label={tool.label} />
      <button
        type="button"
        className={`tool-toggle-row tool-toggle-row--pick${enabled ? " tool-toggle-row--on" : ""}${lockToneClass("tool-toggle-row", tone)}`}
        data-tool-name={tool.name}
        data-lock-tone={tone ?? undefined}
        aria-pressed={enabled}
        aria-label={enabled
          ? t("{label}已启用", "{label} enabled", { label: tool.label })
          : t("{label}已关闭", "{label} disabled", { label: tool.label })}
        title={tone ? lockHints?.[tone] : undefined}
        disabled={tone === "hard"}
        onClick={() => onToggle(tool.name, !enabled)}
      >
        <span><strong>{tool.label}</strong></span>
        <ToolMarks tool={tool} />
        {tone
          ? <LockMark tone={tone} className="tool-toggle-row__mark" />
          : <Mark className="tool-toggle-row__mark" size={14} aria-hidden="true" />}
      </button>
    </div>
  );
}
