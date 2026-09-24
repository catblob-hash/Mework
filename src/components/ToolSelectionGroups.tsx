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
import type {
  DecisionParameterModes,
  RememberedDecisionForms,
  RememberedToolFamilies,
  ToolDescriptor
} from "../types";
import { ToolDocsLink, ToolFamilyDocsLink } from "./DocsLink";
import {
  familyRowCounts,
  familyRowNames,
  familyRowsOn,
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
 * row per family: the row switches the family on and off, and the gear at its
 * end opens the family's own settings window, where its tools are chosen one
 * by one. The stored list keeps the real names: nothing downstream of this
 * component has ever heard of `files`, `shell` or `preview`.
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

/** A family's window, and what it is editing. */
interface OpenFamily {
  id: ToolFamilyId;
  /**
   * The family was off when the window opened, so the window edits the rows
   * switching the family on would use — its remembered ones — rather than live
   * tools. Fixed for the window's life: turning every row off in a window that
   * opened on live tools switches the family off, it does not start parking.
   */
  parked: boolean;
  /**
   * The window was opened by a click on the row of a family that was off with
   * nothing remembered: closing it switches on whatever it chose.
   */
  enableOnClose: boolean;
}

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
  rememberedToolFamilies,
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
   * The rows each switched-off family had on, which switching it back on
   * returns to. Kept by an owner with `onToolSettingsChange`; any other owner's
   * are held here for as long as the picker is on screen.
   */
  rememberedToolFamilies?: RememberedToolFamilies;
  /**
   * A tool-list change with the modes, miss scoring and remembered choices
   * that go with it, written as one edit. Without it, only the list is written.
   */
  onToolSettingsChange?: (next: DecisionToolSettings) => void;
}) {
  const { t } = useI18n();
  const instanceId = useId().replace(/:/g, "");
  const [openFamily, setOpenFamily] = useState<OpenFamily | null>(null);
  /* A role's tool list has nowhere to keep a family's rows, so they live here. */
  const [heldFamilies, setHeldFamilies] = useState<RememberedToolFamilies>({});
  const rememberedFamilies = onToolSettingsChange ? rememberedToolFamilies ?? {} : heldFamilies;
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
    rememberedDecisionForms: rememberedDecisionForms ?? {},
    rememberedToolFamilies: rememberedFamilies
  };
  const write = (next: DecisionToolSettings) => {
    if (onToolSettingsChange) {
      onToolSettingsChange(next);
      return;
    }
    // A parked window's edit moves no tool, and a list-only owner has nothing else to hear.
    const moved = next.enabledTools.length !== enabledTools.length
      || next.enabledTools.some((name, index) => name !== enabledTools[index]);
    if (moved) onChange(next.enabledTools);
    if (next.rememberedToolFamilies) setHeldFamilies(next.rememberedToolFamilies);
  };
  /** Whether a name is on in `names` and free to move: a locked one is neither on nor off by choice. */
  const freelyOn = (names: readonly string[]) => (name: string) => names.includes(name) && !lockedRealNames.has(name);
  /** Whether a name is on in `names`, a locked one included. */
  const onIn = (names: readonly string[]) => (name: string) => names.includes(name) || lockedRealNames.has(name);
  /** The rows of a family switching it on would bring back, as far as the catalog still has them. */
  const rememberedRows = (present: PresentFamily): string[] => {
    const rows = new Set(rememberedFamilies[present.family.id] ?? []);
    return familyRowsOn(present.family, present.nameSet, (name) => rows.has(name));
  };
  /**
   * `after` with each family's rows remembered as it goes off, and forgotten
   * as it comes on. Only free rows count either way: a locked tool stays on
   * whatever the row says, so it is neither what switching off put away nor
   * what switching on brought back.
   */
  const withFamilyMemory = (before: DecisionToolSettings, after: DecisionToolSettings): DecisionToolSettings => {
    const memory: RememberedToolFamilies = { ...(after.rememberedToolFamilies ?? {}) };
    for (const present of families) {
      const wasOn = familyRowsOn(present.family, present.nameSet, freelyOn(before.enabledTools));
      const nowOn = familyRowsOn(present.family, present.nameSet, freelyOn(after.enabledTools));
      if (nowOn.length) delete memory[present.family.id];
      else if (wasOn.length) memory[present.family.id] = wasOn;
    }
    return { ...after, rememberedToolFamilies: memory };
  };
  /**
   * Writes a narrowed tool list. A decision tool that leaves keeps its form and
   * miss scoring among the remembered choices, and a family that leaves keeps
   * its rows, so switching either back on returns to them.
   */
  const removeTools = (removed: ReadonlySet<string>) =>
    write(withFamilyMemory(settings, switchToolsOff(settings, removed)));
  /** Writes a widened tool list, each row coming back to the choices it was switched off with. */
  const addTools = (rows: readonly string[]) =>
    write(withFamilyMemory(settings, switchToolsOn(settings, rows, availableNames)));
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
    // Another owner's list: what this one held for its families is not that one's.
    setHeldFamilies({});
    setOpenFamily(null);
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

  /**
   * A family row's click. A family with rows free to move switches off, keeping
   * them; one that is off comes back on with what it kept. One with nothing
   * kept has nothing to come back with, so the click opens its window instead,
   * and whatever the window chooses is switched on when it closes.
   */
  const toggleFamily = (present: PresentFamily) => {
    const freeRows = familyRowsOn(present.family, present.nameSet, freelyOn(enabledTools));
    if (freeRows.length) {
      const names = freeRows.flatMap((row) => familyRowNames(present.family, row, present.nameSet));
      removeTools(new Set(names.filter((name) => !lockedRealNames.has(name))));
      return;
    }
    const remembered = rememberedRows(present);
    if (remembered.length) {
      addTools(remembered);
      return;
    }
    const anyOn = familyRowsOn(present.family, present.nameSet, onIn(enabledTools)).length > 0;
    setOpenFamily({ id: present.family.id, parked: !anyOn, enableOnClose: !anyOn });
  };

  /** The gear: the family's window, on its live tools while it is on and on the rows it keeps while it is off. */
  const configureFamily = (present: PresentFamily) => {
    const anyOn = familyRowsOn(present.family, present.nameSet, onIn(enabledTools)).length > 0;
    setOpenFamily({ id: present.family.id, parked: !anyOn, enableOnClose: false });
  };

  const lockedRegionId = `tool-lock-${instanceId}`;
  /* A family can leave the catalog while its window is open — a workspace moves
     to a machine without that shell — and the window goes with it. */
  const openPresent = families.find((present) => present.family.id === openFamily?.id);
  /* A parked window draws the family as switching it on would leave it, and
     each edit is put away again as the family's kept rows, so nothing it does
     reaches the live list. */
  const parkedSettings = openPresent && openFamily?.parked
    ? switchToolsOn(settings, rememberedRows(openPresent), availableNames)
    : null;
  const windowSettings = parkedSettings ?? settings;
  const writeFromWindow = (next: DecisionToolSettings) => {
    if (!openPresent || !parkedSettings) {
      write(next);
      return;
    }
    const rows = familyRowsOn(openPresent.family, openPresent.nameSet, (name) => next.enabledTools.includes(name));
    const parked = switchToolsOff(next, openPresent.nameSet);
    const memory: RememberedToolFamilies = { ...rememberedFamilies };
    if (rows.length) memory[openPresent.family.id] = rows;
    else delete memory[openPresent.family.id];
    write({ ...parked, rememberedToolFamilies: memory });
  };
  const closeFamily = () => {
    if (openPresent && openFamily?.enableOnClose) {
      const remembered = rememberedRows(openPresent);
      if (remembered.length) addTools(remembered);
    }
    setOpenFamily(null);
  };
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
                  const on = familyRowsOn(present.family, present.nameSet, onIn(enabledTools)).length > 0;
                  /* While the family is off, its count is the rows it keeps:
                     what the row would switch on. */
                  const kept = new Set(rememberedRows(present));
                  const counts = familyRowCounts(
                    present.family,
                    present.nameSet,
                    on ? onIn(enabledTools) : (name) => kept.has(name)
                  );
                  return (
                    <ToolFamilyRow
                      key={tool.name}
                      family={present.family}
                      dangerous={tool.dangerous}
                      on={on}
                      switchesOff={familyRowsOn(present.family, present.nameSet, freelyOn(enabledTools)).length > 0}
                      enabledCount={counts.enabled}
                      total={counts.total}
                      onToggle={() => toggleFamily(present)}
                      onConfigure={() => configureFamily(present)}
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
          enabledTools={windowSettings.enabledTools}
          lockedTools={lockedTools}
          decisionParameterModes={onToolSettingsChange ? windowSettings.decisionParameterModes : undefined}
          lockedDecisionParameterModes={lockedDecisionParameterModes}
          decisionMissScoring={windowSettings.decisionMissScoring}
          rememberedDecisionForms={windowSettings.rememberedDecisionForms}
          onChange={writeFromWindow}
          onClose={closeFamily}
          notice={!openFamily?.parked
            ? undefined
            : openFamily.enableOnClose
              ? t(
                "这组工具目前关闭。关闭窗口时，这里选中的工具随即启用；一个都不选，这组工具保持关闭。",
                "This group is off. When this window closes, the tools chosen here are switched on; with none chosen, the group stays off."
              )
              : t(
                "这组工具目前关闭。这里选中的工具会在打开这组工具时启用。",
                "This group is off. The tools chosen here are the ones switching it on will use."
              )}
        />
      )}
    </div>
  );
}

/**
 * A tool family's row. The row is the family's switch: it fills in while any
 * of the family's tools is on, and its sign says which way a click moves it.
 * The gear at its end is the way into the window where the tools behind it are
 * chosen one by one. The count is the window's: how many of its rows are on —
 * or, while the family is off, how many switching it on would bring back.
 */
function ToolFamilyRow({
  family,
  dangerous,
  on,
  switchesOff,
  enabledCount,
  total,
  onToggle,
  onConfigure
}: {
  family: ToolFamily;
  dangerous: boolean;
  /** Any of the family's tools is on, a locked one included. */
  on: boolean;
  /** A click would switch tools off: some that are on are free to move. */
  switchesOff: boolean;
  enabledCount: number;
  total: number;
  onToggle: () => void;
  onConfigure: () => void;
}) {
  const { t } = useI18n();
  const Mark = switchesOff ? Minus : Plus;
  const label = family.title(t);
  return (
    <div className="tool-pick-row">
      {/* The row stands for the whole family, so its link goes to the head of
          the family's section; each tool's own page is linked from the window. */}
      <ToolFamilyDocsLink family={family.id} label={label} />
      <div
        className={`tool-toggle-row tool-toggle-row--pick tool-family-row${on ? " tool-toggle-row--on" : ""}`}
        data-tool-name={family.name}
      >
        <button
          type="button"
          className="tool-family-row__switch"
          aria-pressed={on}
          aria-label={on
            ? t("{label}已启用，{enabled} / {total}", "{label} enabled, {enabled} of {total}", {
              label, enabled: enabledCount, total
            })
            : t("{label}已关闭，已选 {enabled} / {total}", "{label} disabled, {enabled} of {total} chosen", {
              label, enabled: enabledCount, total
            })}
          onClick={onToggle}
        >
          <span><strong>{family.name}</strong></span>
          <small className="tool-toggle-row__count">{enabledCount} / {total}</small>
          {dangerous && <em>{t("需审查", "Reviewed")}</em>}
          <Mark className="tool-toggle-row__mark" size={14} aria-hidden="true" />
        </button>
        <button
          type="button"
          className="tool-family-row__settings"
          aria-haspopup="dialog"
          aria-label={family.settingsLabel(t)}
          title={family.settingsLabel(t)}
          onClick={onConfigure}
        >
          <Settings2 className="tool-toggle-row__mark" size={14} aria-hidden="true" />
        </button>
      </div>
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
