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
  Plus,
  Settings2
} from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import {
  type DecisionToolSettings,
  switchToolsOff,
  switchToolsOn
} from "../lib/decisionParameters";
import { isDecisionToolName, isHostDerivedToolName } from "../lib/taskTools";
import type { DecisionParameterModes, RememberedDecisionForms, ToolDescriptor } from "../types";
import { ToolDocsLink } from "./DocsLink";
import {
  familyRowCounts,
  familyVariantNames,
  TOOL_FAMILIES,
  ToolFamilySettingsDialog,
  type ToolFamily,
  type ToolFamilyId
} from "./ToolFamilySettings";

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

/**
 * A tool family as this picker's catalog has it: the members it found, and the
 * names its one row stands for.
 *
 * The files, the shells and the preview are each a single capability split into
 * many calls for the model's benefit, not the reader's. So the picker shows one
 * row per family, and that row opens the family's own settings window. The
 * stored list keeps the real names: nothing downstream of this component has
 * ever heard of `files`, `shell` or `preview`.
 */
interface PresentFamily {
  family: ToolFamily;
  members: ToolDescriptor[];
  names: string[];
  nameSet: ReadonlySet<string>;
  /** What the row switches on in bulk: every member but the variants, which are
   * choices made in the window rather than tools of their own. */
  baseNames: string[];
}

function familyDescriptor({ family, members }: PresentFamily): ToolDescriptor {
  return {
    name: family.name,
    label: family.name,
    description: "",
    category: family.category,
    dangerous: members.some((tool) => tool.dangerous),
    parameters: []
  };
}

type Translate = ReturnType<typeof useI18n>["t"];

/** The fallback is not type-protected. Adding a `ToolCategory` makes `groupMeta`
 * fail through `satisfies`, but this function must be updated explicitly too. */
export function groupLabel(category: ToolCategory, t: Translate): string {
  if (category === "filesystem") return t("文件与搜索", "Files and search");
  if (category === "web") return t("操控", "Control");
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
  lockedTools = [],
  decisionParameterModes,
  lockedDecisionParameterModes,
  decisionMissScoring,
  rememberedDecisionForms,
  onToolSettingsChange
}: {
  tools: ToolDescriptor[];
  enabledTools: string[];
  onChange: (enabledTools: string[]) => void;
  expansionKey: string;
  /**
   * Tools a run has already put in front of the model. They leave the picker for
   * a single collapsed bar, so what remains below is exactly the surface still
   * free to move. A family's tools are the exception: its row stays, and the
   * window it opens draws each spent tool as a switch that cannot be turned off.
   */
  lockedTools?: readonly string[];
  /**
   * The conversation's decision-parameter modes, edited in the family windows.
   * Omitted — together with `onToolSettingsChange` — where the owner has no
   * modes of its own, such as a role's tool list, which hides those options
   * and keeps no choices for tools switched off.
   */
  decisionParameterModes?: DecisionParameterModes;
  lockedDecisionParameterModes?: DecisionParameterModes;
  /** The element tools whose misses the conversation scores, edited beside the modes. */
  decisionMissScoring?: readonly string[];
  /** The sub-option choices the conversation keeps for its switched-off tools. */
  rememberedDecisionForms?: RememberedDecisionForms;
  /**
   * A tool-list change with the modes, miss scoring and remembered choices
   * that go with it, written as one edit. Without it, only the list is written.
   */
  onToolSettingsChange?: (next: DecisionToolSettings) => void;
}) {
  const { t } = useI18n();
  const instanceId = useId().replace(/:/g, "");
  const [openFamily, setOpenFamily] = useState<ToolFamilyId | null>(null);
  const families = useMemo((): PresentFamily[] => {
    const catalog = uniqueTools(tools);
    return TOOL_FAMILIES.flatMap((family) => {
      const members = catalog.filter((tool) => family.owns(tool.name));
      if (!members.length) return [];
      const names = members.map((tool) => tool.name);
      const variants = familyVariantNames(family);
      return [{
        family,
        members,
        names,
        nameSet: new Set(names),
        baseNames: names.filter((name) => !variants.has(name))
      }];
    });
  }, [tools]);
  const familyByRow = useMemo(
    () => new Map(families.map((present) => [present.family.name, present])),
    [families]
  );
  /** A stored list as the picker shows it: a family's names stand for its one row. */
  const fold = useCallback((names: readonly string[]): string[] => {
    const folded = names.filter((name) => !families.some((present) => present.nameSet.has(name)));
    for (const present of families) {
      if (names.some((name) => present.nameSet.has(name))) folded.push(present.family.name);
    }
    return folded;
  }, [families]);
  const foldedEnabled = useMemo(() => fold(enabledTools), [enabledTools, fold]);
  /**
   * A family's one row, back as the names it stands for: the base tools when
   * switching on, every member when switching off.
   */
  const expandNames = useCallback((candidates: readonly string[], removing = false): string[] => candidates.flatMap(
    (candidate) => {
      const present = familyByRow.get(candidate);
      if (!present) return [candidate];
      return removing ? present.names : present.baseNames;
    }
  ), [familyByRow]);
  /* A family's row never moves to the lock bar: it is how the family's spent
     tools are seen at all, and how the ones still free are reached. */
  const lockedNames = useMemo(
    () => new Set(fold(lockedTools).filter((name) => !familyByRow.has(name))),
    [familyByRow, fold, lockedTools]
  );
  const lockedRealNames = useMemo(() => new Set(lockedTools), [lockedTools]);
  const availableNames = useMemo(() => new Set(tools.map((tool) => tool.name)), [tools]);
  const settings: DecisionToolSettings = {
    enabledTools,
    decisionParameterModes: decisionParameterModes ?? {},
    decisionMissScoring: [...(decisionMissScoring ?? [])],
    rememberedDecisionForms: rememberedDecisionForms ?? {}
  };
  const write = (next: DecisionToolSettings) => {
    if (onToolSettingsChange) onToolSettingsChange(next);
    else onChange(next.enabledTools);
  };
  /**
   * Writes a narrowed tool list. A decision tool that leaves keeps its form and
   * miss scoring among the remembered choices, so switching it back on returns
   * to them.
   */
  const removeTools = (removed: ReadonlySet<string>) => write(switchToolsOff(settings, removed));
  /** Writes a widened tool list, each row coming back to the choices it was switched off with. */
  const addTools = (rows: readonly string[]) => write(switchToolsOn(settings, rows, availableNames));
  const groupedTools = useMemo(() => {
    const deduplicated = uniqueTools(tools).filter(
      // Memory, task-runtime, and skill tools are host-derived and cannot be
      // individually toggled. Category filtering is not a substitute for
      // `isHostDerivedToolName`.
      (tool) => tool.category !== "memory" && !isHostDerivedToolName(tool.name)
    );
    // A family's row takes the place of the first of its members the catalog lists.
    const drawn = new Set<ToolFamilyId>();
    const pickable = deduplicated.flatMap((tool): ToolDescriptor[] => {
      const present = families.find((candidate) => candidate.nameSet.has(tool.name));
      if (!present) return [tool];
      if (drawn.has(present.family.id)) return [];
      drawn.add(present.family.id);
      return [familyDescriptor(present)];
    });
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
  }, [families, tools]);
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
      addTools(expandNames([name]));
      return;
    }
    // Disabling a parent also removes dependents so no unreachable toggle or
    // inaccurate count remains. Locked names survive either way: the model has
    // already been handed them and the transcript may already call them.
    const removed = new Set(
      expandNames([name, ...(NESTED_TOOLS[name] ?? [])], true).filter((tool) => !lockedRealNames.has(tool))
    );
    if (!removed.size) return;
    removeTools(removed);
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
    const candidates = group.allTools.filter((tool) => !lockedNames.has(tool.name)).map((tool) => tool.name);
    const names = expandNames(candidates, !enabled);
    if (enabled) {
      // Turning a group on is also a way of asking to see it: a collapsed
      // group would otherwise report a new count with nothing to show for it.
      setCollapsedGroups((current) => {
        if (!current.has(category)) return current;
        const next = new Set(current);
        next.delete(category);
        return next;
      });
      addTools(names);
      return;
    }
    // Turning one off has no second job, so it leaves the disclosure alone.
    const removed = new Set(names.filter((name) => !lockedRealNames.has(name)));
    if (!removed.size) return;
    removeTools(removed);
  };

  const lockedRegionId = `tool-lock-${instanceId}`;
  /* A family can leave the catalog while its window is open — a workspace moves
     to a machine without that shell — and the window goes with it. */
  const openPresent = families.find((present) => present.family.id === openFamily);
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
        const enabledCount = pickable.filter((tool) => foldedEnabled.includes(tool.name)).length;
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
                {groupTools.map((tool) => {
                  const present = familyByRow.get(tool.name);
                  if (!present) {
                    return (
                      <ToolToggle
                        key={tool.name}
                        tool={tool}
                        enabledTools={foldedEnabled}
                        lockedNames={lockedNames}
                        onToggle={toggleTool}
                        nested={allTools.filter((candidate) =>
                          (NESTED_TOOLS[tool.name] ?? []).includes(candidate.name))}
                      />
                    );
                  }
                  const counts = familyRowCounts(
                    present.family,
                    present.nameSet,
                    (name) => enabledTools.includes(name) || lockedRealNames.has(name)
                  );
                  return (
                    <ToolFamilyRow
                      key={tool.name}
                      family={present.family}
                      dangerous={tool.dangerous}
                      enabledCount={counts.enabled}
                      total={counts.total}
                      onOpen={() => setOpenFamily(present.family.id)}
                    />
                  );
                })}
              </div>
            </div>
          </div>
        );
      })}
      {openPresent && (
        <ToolFamilySettingsDialog
          family={openPresent.family}
          tools={openPresent.members}
          enabledTools={enabledTools}
          lockedTools={lockedTools}
          decisionParameterModes={onToolSettingsChange ? decisionParameterModes ?? {} : undefined}
          lockedDecisionParameterModes={lockedDecisionParameterModes}
          decisionMissScoring={decisionMissScoring ?? []}
          rememberedDecisionForms={rememberedDecisionForms}
          onChange={write}
          onClose={() => setOpenFamily(null)}
        />
      )}
    </div>
  );
}

/**
 * A tool family's row. It holds no switch of its own: the tools behind it are
 * chosen one by one in the window its trailing settings button opens, so the
 * row reports how many of that window's rows are on and is otherwise the way in.
 */
function ToolFamilyRow({
  family,
  dangerous,
  enabledCount,
  total,
  onOpen
}: {
  family: ToolFamily;
  dangerous: boolean;
  enabledCount: number;
  total: number;
  onOpen: () => void;
}) {
  const { t } = useI18n();
  const on = enabledCount > 0;
  const Icon = family.icon;
  return (
    <div className="tool-pick-row">
      {/* The documentation link's slot, kept so the label reads down the same
          column as every other tool's; the family's tools link their own pages
          from the window. */}
      <span className="tool-docs-link" aria-hidden="true"><Icon size={14} /></span>
      <button
        type="button"
        className={`tool-toggle-row tool-toggle-row--pick${on ? " tool-toggle-row--on" : ""}`}
        data-tool-name={family.name}
        aria-haspopup="dialog"
        aria-label={t(
          "{settings}，已启用 {enabled} / {total}",
          "{settings}, {enabled} of {total} enabled",
          { settings: family.settingsLabel(t), enabled: enabledCount, total }
        )}
        onClick={onOpen}
      >
        <span><strong>{family.name}</strong></span>
        <small className="tool-toggle-row__count">{enabledCount} / {total}</small>
        {dangerous && <em>{t("需审查", "Reviewed")}</em>}
        <Settings2 className="tool-toggle-row__mark" size={14} aria-hidden="true" />
      </button>
    </div>
  );
}

/**
 * The marks a tool row carries before its trailing sign: a blue one when the
 * tool runs through the decision model — its calls send content to the
 * decision model provider — and the amber one when its calls are reviewed.
 */
function ToolMarks({ tool }: { tool: ToolDescriptor }) {
  const { t } = useI18n();
  return (
    <>
      {isDecisionToolName(tool.name) && (
        <em className="tool-toggle-row__decision">{t("决策模型", "Decision model")}</em>
      )}
      {tool.dangerous && <em>{t("需审查", "Reviewed")}</em>}
    </>
  );
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
